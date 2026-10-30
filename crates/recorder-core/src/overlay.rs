//! Burn-in overlay: text blocks whose content is a template filled from live data.
//!
//! Data flow: a [`DataSource`] pushes key/value pairs into the [`DataBus`]; a ticker renders each
//! block's template into a small text file; FFmpeg's `drawtext` re-reads that file every frame
//! (`reload=1`). No frames cross a pipe and the rendering cost is a few file writes per second.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    #[default]
    Text,
    Image,
}

/// One object placed on the frame, like the objects of the Xspector overlay editor.
/// Positions and sizes are fractions of the frame (0..1) so they hold at any resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayItem {
    pub id: String,
    pub enabled: bool,
    pub kind: ItemKind,
    /// Top-left corner, fraction of frame width / height.
    pub x: f32,
    pub y: f32,
    /// Text template; `{key}` is replaced by live data. Built-ins: `{time}` `{date}` `{clock}` `{ch}`.
    pub text: String,
    /// Font height as a fraction of frame height.
    pub size: f32,
    /// `#RRGGBB`
    pub color: String,
    pub bold: bool,
    pub bg_color: String,
    /// 0 = no box behind the text
    pub bg_opacity: f32,
    pub image: Option<PathBuf>,
    /// Image width as a fraction of frame width (height keeps the aspect ratio).
    pub width: f32,
    pub opacity: f32,
}

impl Default for OverlayItem {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            kind: ItemKind::Text,
            x: 0.03,
            y: 0.04,
            text: "Text".into(),
            size: 0.05,
            color: "#FFFFFF".into(),
            bold: false,
            bg_color: "#000000".into(),
            bg_opacity: 0.0,
            image: None,
            width: 0.2,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataSource {
    #[default]
    None,
    /// Fake depth/heading/temperature so the overlay can be tried without hardware.
    Simulator,
    /// JSON objects, one per UDP datagram, e.g. `{"depth": 12.3, "heading": 270}`.
    UdpJson { port: u16 },
}

pub const MAX_ITEMS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMode {
    /// The overlay is blended into the frames by the app (`mixer.rs`): move, restyle, add and remove
    /// objects while recording. Default: measured at the same total CPU as the other modes.
    #[default]
    Layer,
    /// Baked into the FFmpeg filter graph (`drawtext` / `overlay`): fixed once recording starts.
    Burn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub mode: OverlayMode,
    pub font_file: Option<PathBuf>,
    pub items: Vec<OverlayItem>,
    pub source: DataSource,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: OverlayMode::default(),
            font_file: None,
            items: vec![
                OverlayItem { id: "time".into(), text: "CH{ch}  {time}".into(), bg_opacity: 0.5, ..Default::default() },
                OverlayItem {
                    id: "depth".into(),
                    text: "Depth {depth} m   Heading {heading}\u{b0}".into(),
                    y: 0.9,
                    bg_opacity: 0.5,
                    ..Default::default()
                },
            ],
            source: DataSource::Simulator,
        }
    }
}

impl OverlayConfig {
    /// Items that become text files (index = position in `items`, kept stable for file names).
    pub fn text_items(&self) -> impl Iterator<Item = (usize, &OverlayItem)> {
        self.items.iter().enumerate().filter(|(_, i)| i.enabled && i.kind == ItemKind::Text)
    }
}

/// Latest value per key. Cheap to clone (shared).
#[derive(Debug, Clone, Default)]
pub struct DataBus(Arc<Mutex<BTreeMap<String, String>>>);

impl DataBus {
    pub fn set(&self, key: impl Into<String>, value: impl Into<String>) {
        self.0.lock().unwrap().insert(key.into(), value.into());
    }
    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.0.lock().unwrap().clone()
    }
}

pub fn fmt_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e12 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Flatten a JSON object into `key -> text`; nested keys become `a.b`.
pub fn flatten_json(prefix: &str, v: &serde_json::Value, out: &mut Vec<(String, String)>) {
    use serde_json::Value::*;
    match v {
        Object(m) => {
            for (k, x) in m {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten_json(&key, x, out);
            }
        }
        Number(n) => out.push((prefix.into(), n.as_f64().map_or(n.to_string(), fmt_number))),
        String(s) => out.push((prefix.into(), s.clone())),
        Bool(b) => out.push((prefix.into(), b.to_string())),
        _ => {}
    }
}

/// Fill `{key}` placeholders. Unknown keys show `--` so a missing sensor is visible, not blank.
pub fn render(template: &str, channel: u32, data: &BTreeMap<String, String>) -> String {
    let now = Local::now();
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = after[..end].trim();
                match key {
                    "time" => out.push_str(&now.format("%Y-%m-%d %H:%M:%S").to_string()),
                    "date" => out.push_str(&now.format("%Y-%m-%d").to_string()),
                    "clock" => out.push_str(&now.format("%H:%M:%S").to_string()),
                    "clock_ms" => out.push_str(&now.format("%H:%M:%S%.3f").to_string()),
                    "ch" => out.push_str(&channel.to_string()),
                    k => out.push_str(data.get(k).map_or("--", String::as_str)),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn text_file(dir: &Path, block: usize, channel: u32) -> PathBuf {
    dir.join(format!("b{block}_ch{channel}.txt"))
}

/// Write (atomically) only when the text changed. Errors are ignored: the next tick retries and
/// FFmpeg keeps showing the previous text meanwhile.
async fn write_if_changed(path: &Path, text: &str, last: &mut String) {
    if last == text {
        return;
    }
    let tmp = path.with_extension("tmp");
    if tokio::fs::write(&tmp, text).await.is_ok() && tokio::fs::rename(&tmp, path).await.is_ok() {
        *last = text.to_string();
    }
}

/// Create the initial text files (FFmpeg needs them before it starts), then keep them current.
pub async fn start_writer(dir: PathBuf, items: Vec<(usize, OverlayItem)>, channels: Vec<u32>, bus: DataBus) -> JoinHandle<()> {
    let mut last: Vec<Vec<String>> = vec![vec![String::new(); channels.len()]; items.len()];
    let data = bus.snapshot();
    for (n, (idx, it)) in items.iter().enumerate() {
        for (ci, &ch) in channels.iter().enumerate() {
            write_if_changed(&text_file(&dir, *idx, ch), &render(&it.text, ch, &data), &mut last[n][ci]).await;
        }
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            let data = bus.snapshot();
            for (n, (idx, it)) in items.iter().enumerate() {
                for (ci, &ch) in channels.iter().enumerate() {
                    write_if_changed(&text_file(&dir, *idx, ch), &render(&it.text, ch, &data), &mut last[n][ci]).await;
                }
            }
        }
    })
}

pub fn start_source(source: &DataSource, bus: DataBus) -> Option<JoinHandle<()>> {
    match source {
        DataSource::None => None,
        DataSource::Simulator => Some(tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                let t = t0.elapsed().as_secs_f64();
                bus.set("depth", fmt_number(20.0 + 15.0 * (t / 20.0).sin()));
                bus.set("heading", fmt_number((t * 6.0) % 360.0));
                bus.set("temp", fmt_number(18.0 + 0.5 * (t / 7.0).cos()));
                bus.set("pitch", fmt_number(3.0 * (t / 3.0).sin()));
                bus.set("roll", fmt_number(2.0 * (t / 4.0).cos()));
            }
        })),
        DataSource::UdpJson { port } => {
            let addr = SocketAddr::from(([0, 0, 0, 0], *port));
            Some(tokio::spawn(async move {
                let Ok(sock) = UdpSocket::bind(addr).await else {
                    bus.set("_error", format!("cannot listen on UDP {}", addr.port()));
                    return;
                };
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    let Ok((n, _)) = sock.recv_from(&mut buf).await else { continue };
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) {
                        let mut kv = vec![];
                        flatten_json("", &v, &mut kv);
                        for (k, val) in kv {
                            bus.set(k, val);
                        }
                    }
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_fills_known_and_marks_missing() {
        let mut d = BTreeMap::new();
        d.insert("depth".into(), "12.5".into());
        assert_eq!(render("D {depth} H {heading} CH{ch}", 2, &d), "D 12.5 H -- CH2");
        assert_eq!(render("no braces", 1, &d), "no braces");
        assert_eq!(render("open { brace", 1, &d), "open { brace");
    }

    #[test]
    fn numbers_are_compact() {
        assert_eq!(fmt_number(12.0), "12");
        assert_eq!(fmt_number(12.340), "12.34");
        assert_eq!(fmt_number(12.3456), "12.35");
    }

    #[test]
    fn flatten_nested_json() {
        let v: serde_json::Value = serde_json::from_str(r#"{"depth":12.5,"imu":{"yaw":90},"ok":true}"#).unwrap();
        let mut out = vec![];
        flatten_json("", &v, &mut out);
        out.sort();
        assert_eq!(out, vec![("depth".into(), "12.5".into()), ("imu.yaw".into(), "90".into()), ("ok".into(), "true".into())]);
    }
}
