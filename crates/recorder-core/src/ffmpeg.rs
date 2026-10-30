use std::path::Path;

use crate::config::{ChannelConfig, Deinterlace, RecordConfig, Source};
use crate::overlay::{self, ItemKind, OverlayMode};
use crate::preview::StreamKind;
use crate::probe::Encoder;


/// Where and how FFmpeg sends the live preview.
#[derive(Debug, Clone, Copy)]
pub struct PreviewSpec {
    pub port: u16,
    pub kind: StreamKind,
}

/// Input options for the camera (or the synthetic test pattern).
fn input_args(cfg: &RecordConfig, ch: &ChannelConfig) -> Vec<String> {
    let mut a: Vec<String> = vec![];
    match &ch.source {
        Source::TestPattern => {
            let size = ch.size.as_deref().unwrap_or("1280x720");
            a.extend(["-re".into(), "-f".into(), "lavfi".into(), "-i".into()]);
            a.push(format!("testsrc2=size={size}:rate={}", ch.fps));
        }
        Source::Device(name) => {
            if cfg!(target_os = "macos") {
                a.extend(["-f".into(), "avfoundation".into()]);
                if cfg.preview_only {
                    a.extend(["-fflags".into(), "nobuffer".into()]); // lowest latency for the live view
                }
                a.extend(["-framerate".into(), ch.fps.to_string()]);
                if let Some(s) = &ch.size {
                    a.extend(["-video_size".into(), s.clone()]);
                }
                a.extend(["-i".into(), format!("{name}:none")]);
            } else {
                a.extend(["-f".into(), "dshow".into()]);
                a.extend(["-framerate".into(), ch.fps.to_string()]);
                if let Some(s) = &ch.size {
                    a.extend(["-video_size".into(), s.clone()]);
                }
                a.extend(["-i".into(), format!("video={name}")]);
            }
        }
    }
    a
}

fn base_flags(level: &str) -> Vec<String> {
    ["-hide_banner", "-y", "-loglevel", level, "-stats", "-stats_period", "1"].map(String::from).into()
}

/// Encoder, keyframes and the segment muxer: fragmented MP4 pieces written into `staging_pattern`
/// (`.../CH1_p0_%05d.mp4`), with one CSV line per *finished* segment on stdout. Fragmented MP4 keeps
/// a partially written file playable after a crash or power loss.
fn encode_tail(a: &mut Vec<String>, cfg: &RecordConfig, ch: &ChannelConfig, enc: Encoder, staging_pattern: &Path) {
    a.extend(["-an".into(), "-pix_fmt".into(), "yuv420p".into()]);
    a.extend(enc.args(&cfg.bitrate));
    // Keyframe every 2 s so segment cuts land close to the requested duration.
    a.extend(["-g".into(), (ch.fps * 2).to_string()]);
    a.extend(
        [
            "-f", "segment",
            "-segment_time", &cfg.segment_secs.to_string(),
            "-segment_format", "mp4",
            "-segment_format_options", "movflags=+frag_keyframe+empty_moov+default_base_moof",
            "-reset_timestamps", "1",
            "-segment_list", "pipe:1",
            "-segment_list_type", "csv",
        ]
        .map(String::from),
    );
    a.push(staging_pattern.to_string_lossy().into_owned());
}

/// Build the FFmpeg argument list for one channel process that captures, filters, previews and
/// records in a single graph (no live overlay layer; see [`build_capture_args`] for that).
pub fn build_args(
    cfg: &RecordConfig,
    ch: &ChannelConfig,
    enc: Encoder,
    staging_pattern: &Path,
    preview: Option<&PreviewSpec>,
) -> Vec<String> {
    // The live monitor logs at `info` once so its banner reports the device's frame size.
    let mut a = base_flags(if cfg.preview_only { "info" } else { "error" });
    a.extend(input_args(cfg, ch));

    // ---- filter graph ---------------------------------------------------------------------
    // The preview is taken *before* the overlay is applied: the UI draws the overlay itself so it
    // can be edited live, and the recording gets the real burn-in.
    // Every frame up to 30 per second: dropping frames unevenly (30 -> 20) looks like judder.
    let fps_cap = if ch.fps > 30 { "fps=30," } else { "" };
    let preview_chain = match preview.map(|p| p.kind) {
        // Camera-native packed/semi-planar frames: no scaling and no conversion on the CPU.
        Some(StreamKind::Raw { fmt, .. }) => format!("{fps_cap}format={}[pvo]", fmt.pix_fmt()),
        _ => format!("{fps_cap}scale=w='min({},iw)':h=-2:flags=fast_bilinear,format=yuv420p[pvo]", cfg.preview_max_width),
    };
    let mut parts: Vec<String> = vec![];
    let mut cur = "[0:v]".to_string();
    if ch.deinterlace == Deinterlace::On {
        // bwdif, not yadif: yadif is GPL-only and about 7x more expensive here (measured).
        parts.push(format!("{cur}bwdif=mode=send_frame[d]"));
        cur = "[d]".into();
    }

    // Live monitor: no encoder, no segments, just the small preview.
    if let (true, Some(dir)) = (cfg.preview_only, &cfg.preview_dir) {
        parts.push(format!("{cur}{preview_chain}"));
        a.extend(["-filter_complex".into(), parts.join(";"), "-map".into(), "[pvo]".into(), "-an".into()]);
        preview_output(&mut a, dir, ch.id, preview);
        return a;
    }
    if cfg.preview_dir.is_some() {
        // One decode, two outputs: the recording keeps full quality, the preview is a small raw stream.
        parts.push(format!("{cur}split=2[rec0][pv]"));
        parts.push(format!("[pv]{preview_chain}"));
        cur = "[rec0]".into();
    }
    if cfg.overlay.mode == OverlayMode::Burn {
        parts.extend(overlay_parts(cfg, ch.id, &mut cur));
    }
    if !parts.is_empty() {
        a.extend(["-filter_complex".into(), parts.join(";"), "-map".into(), cur]);
    }
    encode_tail(&mut a, cfg, ch, enc, staging_pattern);
    if let Some(dir) = &cfg.preview_dir {
        a.extend(["-map", "[pvo]", "-an"].map(String::from));
        preview_output(&mut a, dir, ch.id, preview);
    }
    a
}

/// First half of the live-overlay pipeline: capture (and deinterlace) and hand the frames, as a
/// plain YUV 4:2:0 stream with no frame dropping or repeating, on stdout (the compositor reads it).
pub fn build_capture_args(cfg: &RecordConfig, ch: &ChannelConfig) -> Vec<String> {
    let mut a = base_flags("error");
    a.extend(input_args(cfg, ch));
    let bw = if ch.deinterlace == Deinterlace::On { "bwdif=mode=send_frame," } else { "" };
    a.extend(["-filter_complex".into(), format!("[0:v]{bw}format=yuv420p[c]"), "-map".into(), "[c]".into(), "-an".into()]);
    a.extend(
        ["-c:v", "rawvideo", "-pix_fmt", "yuv420p", "-fps_mode", "passthrough", "-flush_packets", "1", "-f", "yuv4mpegpipe"]
            .map(String::from),
    );
    a.push("pipe:1".into());
    a
}

/// Second half: read the composited frames from stdin (the compositor writes them) and record them.
pub fn build_encoder_args(cfg: &RecordConfig, ch: &ChannelConfig, enc: Encoder, staging_pattern: &Path) -> Vec<String> {
    let mut a = base_flags("error");
    a.extend(["-f".into(), "yuv4mpegpipe".into(), "-i".into(), "pipe:0".into()]);
    encode_tail(&mut a, cfg, ch, enc, staging_pattern);
    a
}

/// The preview output: raw frames to our loopback relay (GPU path), or a JPEG file as a fallback
/// when there is no relay (command-line tools).
fn preview_output(a: &mut Vec<String>, dir: &Path, id: u32, spec: Option<&PreviewSpec>) {
    match spec {
        Some(PreviewSpec { port, kind: StreamKind::Raw { fmt, .. } }) => {
            a.extend(["-c:v", "rawvideo", "-pix_fmt", fmt.pix_fmt(), "-flush_packets", "1", "-f", "rawvideo"].map(String::from));
            a.push(format!("tcp://127.0.0.1:{port}"));
        }
        Some(PreviewSpec { port, kind: StreamKind::Y4m }) => {
            a.extend(["-c:v", "rawvideo", "-pix_fmt", "yuv420p", "-flush_packets", "1", "-f", "yuv4mpegpipe"].map(String::from));
            a.push(format!("tcp://127.0.0.1:{port}"));
        }
        None => {
            a.extend(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v", "8", "-f", "image2", "-update", "1", "-atomic_writing", "1"].map(String::from));
            a.push(dir.join(format!("CH{id}.jpg")).to_string_lossy().into_owned());
        }
    }
}

/// Pixel format name from FFmpeg's input banner: `Stream #0:0: Video: rawvideo (UYVY / 0x..), uyvy422, 1280x720, ...`
pub fn parse_video_pix_fmt(line: &str) -> Option<String> {
    if !line.contains("Stream #0:0") || !line.contains("Video:") {
        return None;
    }
    let toks: Vec<&str> = line.split(", ").collect();
    let i = toks.iter().position(|t| {
        t.split_whitespace().next().and_then(|w| w.split_once('x')).is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok())
    })?;
    let fmt = toks.get(i.checked_sub(1)?)?.split(['(', ' ']).next()?;
    (!fmt.is_empty()).then(|| fmt.to_string())
}

/// Path inside a single-quoted filter option: forward slashes, escaped colons (`C\:/x`).
fn filter_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:")
}

fn hex6(c: &str) -> Option<&str> {
    let c = c.trim_start_matches('#');
    (c.len() == 6 && c.chars().all(|x| x.is_ascii_hexdigit())).then_some(c)
}

/// Filter-graph segments for the overlay items, chained after `cur` (which is advanced).
/// Text needs `overlay_dir` (set only when `drawtext` exists); images only need core filters.
fn overlay_parts(cfg: &RecordConfig, channel: u32, cur: &mut String) -> Vec<String> {
    if !cfg.overlay.enabled {
        return vec![];
    }
    let mut parts = vec![];
    for (i, it) in cfg.overlay.items.iter().enumerate().take(overlay::MAX_ITEMS) {
        if !it.enabled {
            continue;
        }
        let (x, y) = (it.x.clamp(0.0, 1.0), it.y.clamp(0.0, 1.0));
        match it.kind {
            ItemKind::Text => {
                let (Some(dir), Some(color)) = (&cfg.overlay_dir, hex6(&it.color)) else { continue };
                let file = overlay::text_file(dir, i, channel);
                if file.to_string_lossy().contains('\'') {
                    continue; // cannot be quoted safely
                }
                let font = cfg
                    .overlay
                    .font_file
                    .clone()
                    .or_else(|| default_font(it.bold, it.text.chars().any(is_thai)))
                    .map(|f| format!("fontfile='{}':", filter_path(&f)))
                    .unwrap_or_default();
                let bg = match hex6(&it.bg_color) {
                    Some(c) if it.bg_opacity > 0.0 => {
                        format!("box=1:boxcolor=0x{c}@{:.2}:boxborderw=8", it.bg_opacity.clamp(0.0, 1.0))
                    }
                    _ => "box=0".into(),
                };
                parts.push(format!(
                    "{cur}drawtext={font}textfile='{}':reload=1:expansion=none:fontsize=h*{:.4}:fontcolor=0x{color}:x=w*{x:.4}:y=h*{y:.4}:{bg}[t{i}]",
                    filter_path(&file),
                    it.size.clamp(0.01, 0.5),
                ));
                *cur = format!("[t{i}]");
            }
            ItemKind::Image => {
                let Some(path) = it.image.as_ref().filter(|p| p.exists()) else { continue };
                if path.to_string_lossy().contains('\'') {
                    continue;
                }
                parts.push(format!(
                    "movie='{}',format=rgba,colorchannelmixer=aa={:.2}[m{i}]",
                    filter_path(path),
                    it.opacity.clamp(0.0, 1.0)
                ));
                // Scale the picture relative to the video (`rw` = reference width; needs FFmpeg >= 7),
                // keeping the picture's own aspect ratio.
                parts.push(format!("[m{i}]{cur}scale2ref=w=rw*{:.4}:h=ow*main_h/main_w[s{i}][b{i}]", it.width.clamp(0.01, 1.0)));
                parts.push(format!("[b{i}][s{i}]overlay=x=W*{x:.4}:y=H*{y:.4}[o{i}]"));
                *cur = format!("[o{i}]");
            }
        }
    }
    parts
}

/// A font FFmpeg can load directly. Shipped builds should bundle one and set `font_file`.
fn is_thai(c: char) -> bool {
    ('\u{0E00}'..='\u{0E7F}').contains(&c)
}

pub fn default_font(bold: bool, thai: bool) -> Option<std::path::PathBuf> {
    // drawtext does no per-glyph font fallback, so Thai text needs a font that has Thai glyphs.
    if thai {
        let thai_fonts: &[&str] = if cfg!(target_os = "macos") {
            &["/System/Library/Fonts/Supplemental/Ayuthaya.ttf"]
        } else if cfg!(windows) {
            &["C:/Windows/Fonts/tahoma.ttf", "C:/Windows/Fonts/leelawui.ttf"]
        } else {
            &["/usr/share/fonts/truetype/tlwg/Garuda.ttf"]
        };
        if let Some(f) = thai_fonts.iter().map(std::path::PathBuf::from).find(|p| p.exists()) {
            return Some(f);
        }
    }
    let candidates: &[&str] = match (cfg!(target_os = "macos"), cfg!(windows), bold) {
        (true, _, false) => &["/System/Library/Fonts/Supplemental/Arial.ttf", "/System/Library/Fonts/Helvetica.ttc"],
        (true, _, true) => &["/System/Library/Fonts/Supplemental/Arial Bold.ttf", "/System/Library/Fonts/Supplemental/Arial.ttf"],
        (_, true, false) => &["C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/segoeui.ttf"],
        (_, true, true) => &["C:/Windows/Fonts/arialbd.ttf", "C:/Windows/Fonts/arial.ttf"],
        _ => &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
    };
    candidates.iter().map(std::path::PathBuf::from).find(|p| p.exists())
}

/// `frame=  150 fps= 30 q=-1.0 ... drop=0 speed=1.00x`
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Stats {
    pub frame: u64,
    pub drop: u64,
    /// Frames FFmpeg repeated because the input was late (the picture freezes briefly).
    pub dup: u64,
    pub speed: f64,
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.split_once(key)?.1.trim_start();
    rest.split_whitespace().next()
}

pub fn parse_stats(line: &str) -> Option<Stats> {
    if !line.contains("frame=") {
        return None;
    }
    Some(Stats {
        frame: field(line, "frame=")?.parse().ok()?,
        drop: field(line, "drop=").and_then(|v| v.parse().ok()).unwrap_or(0),
        dup: field(line, "dup=").and_then(|v| v.parse().ok()).unwrap_or(0),
        speed: field(line, "speed=")
            .and_then(|v| v.trim_end_matches('x').parse().ok())
            .unwrap_or(0.0),
    })
}

/// A capture mode the device offers: a frame size and the frame rates available at that size.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub fps: Vec<f64>,
}

fn size_tok(t: &str) -> Option<(u32, u32)> {
    let (w, h) = t.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// avfoundation prints `  1280x720@[15.000000 30.000000]fps` after "Supported modes:".
pub fn parse_avfoundation_modes(text: &str) -> Vec<Mode> {
    let mut out: Vec<Mode> = vec![];
    for line in text.lines() {
        let l = line.rsplit("] ").next().unwrap_or(line).trim();
        let Some((size, rest)) = l.split_once("@[") else { continue };
        let Some(sz) = size_tok(size.trim()) else { continue };
        let fps: Vec<f64> = rest.trim_end_matches("fps").trim_end_matches(']').split_whitespace().filter_map(|v| v.parse().ok()).collect();
        if !fps.is_empty() {
            out.push(Mode { w: sz.0, h: sz.1, fps });
        }
    }
    out
}

/// dshow prints ranges: `pixel_format=yuyv422  min s=640x480 fps=5 max s=640x480 fps=30`.
pub fn parse_dshow_modes(text: &str) -> Vec<Mode> {
    const COMMON: [f64; 8] = [5.0, 10.0, 15.0, 24.0, 25.0, 30.0, 50.0, 60.0];
    let mut out: Vec<Mode> = vec![];
    for line in text.lines() {
        let Some(max_part) = line.split("max s=").nth(1) else { continue };
        let Some(min_part) = line.split("min s=").nth(1) else { continue };
        let (Some(max_sz), Some(min_sz)) = (size_tok(max_part.split_whitespace().next().unwrap_or("")), size_tok(min_part.split_whitespace().next().unwrap_or(""))) else { continue };
        let fps_of = |p: &str| p.split("fps=").nth(1).and_then(|v| v.split_whitespace().next()).and_then(|v| v.parse::<f64>().ok());
        let (Some(lo), Some(hi)) = (fps_of(min_part), fps_of(max_part)) else { continue };
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        let mut fps: Vec<f64> = COMMON.iter().copied().filter(|f| *f >= lo - 0.01 && *f <= hi + 0.01).collect();
        if fps.is_empty() {
            fps.push(hi);
        }
        for sz in [max_sz, min_sz] {
            match out.iter_mut().find(|m| (m.w, m.h) == sz) {
                Some(m) => {
                    for f in &fps {
                        if !m.fps.iter().any(|x| (x - f).abs() < 0.01) {
                            m.fps.push(*f);
                        }
                    }
                }
                None => out.push(Mode { w: sz.0, h: sz.1, fps: fps.clone() }),
            }
        }
    }
    for m in &mut out {
        m.fps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Unsupported {
    Framerate,
    Size,
}

/// FFmpeg's complaint when the device cannot do the requested mode.
pub fn unsupported(text: &str) -> Option<Unsupported> {
    if text.contains("Selected framerate") && text.contains("not supported") {
        Some(Unsupported::Framerate)
    } else if text.contains("Selected video size") && text.contains("not supported") {
        Some(Unsupported::Size)
    } else {
        None
    }
}

/// Nearest supported frame rate for `size` (any size when `None`): the highest one not above
/// `want`, else the lowest above it.
pub fn best_fps(modes: &[Mode], size: Option<(u32, u32)>, want: u32, rejected: &[u32]) -> Option<u32> {
    let mut all: Vec<f64> = modes
        .iter()
        .filter(|m| size.is_none_or(|s| (m.w, m.h) == s))
        .flat_map(|m| m.fps.iter().copied())
        .collect();
    if all.is_empty() {
        all = modes.iter().flat_map(|m| m.fps.iter().copied()).collect();
    }
    // The mode list shows min/max of a range; the device may still refuse a listed rate.
    all.retain(|f| !rejected.contains(&(f.round() as u32)));
    let want = want as f64;
    let below = all.iter().copied().filter(|f| *f <= want + 0.01).fold(f64::MIN, f64::max);
    let above = all.iter().copied().filter(|f| *f > want).fold(f64::MAX, f64::min);
    if below > f64::MIN {
        Some(below.round() as u32)
    } else if above < f64::MAX {
        Some(above.round() as u32)
    } else {
        None
    }
}

/// Sensible default when the user did not choose a mode: the largest landscape mode up to about
/// 1280x720 (the camera's own default can be a heavy portrait 1080x1920 on some devices), at the
/// frame rate nearest to `want_fps`.
pub fn pick_default_mode(modes: &[Mode], want_fps: u32) -> Option<((u32, u32), u32)> {
    const TARGET: u64 = 1280 * 720;
    let land: Vec<&Mode> = modes.iter().filter(|m| m.w >= m.h).collect();
    let px = |m: &&Mode| m.w as u64 * m.h as u64;
    let m = land
        .iter()
        .filter(|m| px(m) <= TARGET)
        .max_by_key(|m| px(m))
        .or_else(|| land.iter().min_by_key(|m| px(m)))?;
    let fps = best_fps(std::slice::from_ref(*m), None, want_fps, &[])?;
    Some(((m.w, m.h), fps))
}

/// Input frame size from FFmpeg's banner: `Stream #0:0: Video: rawvideo (...), uyvy422, 720x576, ...`
pub fn parse_video_size(line: &str) -> Option<(u32, u32)> {
    if !line.contains("Stream #0:0") || !line.contains("Video:") {
        return None;
    }
    line.split(|c: char| c == ',' || c.is_whitespace()).find_map(|tok| {
        let (w, h) = tok.split_once('x')?;
        let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
        (w >= 16 && h >= 16).then_some((w, h))
    })
}

/// `CH1_p0_00000.mp4,0.000000,1800.040000` -> (file name, start seconds)
pub fn parse_segment_line(line: &str) -> Option<(String, f64)> {
    let mut it = line.trim().split(',');
    let name = it.next()?.trim();
    let start: f64 = it.next()?.trim().parse().ok()?;
    (!name.is_empty()).then(|| (name.to_string(), start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats() {
        let s = parse_stats("frame=  150 fps= 30 q=-1.0 size=N/A time=00:00:05.00 bitrate=N/A dup=0 drop=3 speed=1.00x")
            .unwrap();
        assert_eq!(s, Stats { frame: 150, drop: 3, dup: 0, speed: 1.0 });
        assert!(parse_stats("some warning").is_none());
    }

    #[test]
    fn segment_line() {
        assert_eq!(
            parse_segment_line("CH1_p0_00000.mp4,0.000000,1800.040000\n"),
            Some(("CH1_p0_00000.mp4".into(), 0.0))
        );
    }

    #[test]
    fn args_end_with_staging_pattern() {
        let cfg = RecordConfig::default();
        let ch = ChannelConfig { id: 1, source: Source::TestPattern, size: None, fps: 30, deinterlace: Deinterlace::On, detected_size: None, detected_format: None };
        let a = build_args(&cfg, &ch, Encoder::X264, Path::new("/s/CH1_p0_%05d.mp4"), None);
        assert_eq!(a.last().unwrap(), "/s/CH1_p0_%05d.mp4");
        assert!(a.iter().any(|x| x.starts_with("[0:v]bwdif")));
        let i = a.iter().position(|x| x == "-segment_time").unwrap();
        assert_eq!(a[i + 1], "1800");
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use crate::overlay::{OverlayConfig, OverlayItem};

    fn cfg(items: Vec<OverlayItem>) -> RecordConfig {
        RecordConfig {
            overlay: OverlayConfig {
                enabled: true,
                mode: OverlayMode::Burn,
                font_file: Some("C:\\Windows\\Fonts\\arial.ttf".into()),
                items,
                ..Default::default()
            },
            overlay_dir: Some("/tmp/ov".into()),
            ..Default::default()
        }
    }

    fn text(x: f32, y: f32) -> OverlayItem {
        OverlayItem { x, y, bg_opacity: 0.5, ..Default::default() }
    }

    #[test]
    fn drawtext_uses_fractions_and_escapes_windows_font() {
        let mut cur = "[0:v]".to_string();
        let p = overlay_parts(&cfg(vec![text(0.25, 0.5)]), 3, &mut cur);
        assert_eq!(p.len(), 1);
        assert!(p[0].starts_with("[0:v]drawtext="), "{}", p[0]);
        assert!(p[0].contains("fontfile='C\\:/Windows/Fonts/arial.ttf'"), "{}", p[0]);
        assert!(p[0].contains("textfile='/tmp/ov/b0_ch3.txt'"));
        assert!(p[0].contains("x=w*0.2500:y=h*0.5000"));
        assert!(p[0].contains("fontsize=h*0.0500"));
        assert!(p[0].contains("boxcolor=0x000000@0.50"));
        assert_eq!(cur, "[t0]");
    }

    #[test]
    fn image_item_is_scaled_to_the_video_and_overlaid() {
        let dir = std::env::temp_dir().join("xs-test-logo.png");
        std::fs::write(&dir, b"x").unwrap();
        let img = OverlayItem { kind: ItemKind::Image, image: Some(dir.clone()), x: 0.7, y: 0.1, width: 0.2, opacity: 0.8, ..Default::default() };
        let mut cur = "[rec0]".to_string();
        let p = overlay_parts(&cfg(vec![img]), 1, &mut cur);
        assert_eq!(p.len(), 3);
        assert!(p[0].contains("colorchannelmixer=aa=0.80"));
        assert!(p[1].starts_with("[m0][rec0]scale2ref=w=rw*0.2000:h=ow*main_h/main_w"));
        assert!(p[2].contains("overlay=x=W*0.7000:y=H*0.1000"));
        assert_eq!(cur, "[o0]");
        let _ = std::fs::remove_file(dir);
    }

    #[test]
    fn disabled_overlay_and_text_without_drawtext_add_nothing() {
        let mut c = cfg(vec![text(0.1, 0.1)]);
        c.overlay.enabled = false;
        assert!(overlay_parts(&c, 1, &mut "[0:v]".to_string()).is_empty());
        let mut c = cfg(vec![text(0.1, 0.1)]);
        c.overlay_dir = None; // drawtext missing
        assert!(overlay_parts(&c, 1, &mut "[0:v]".to_string()).is_empty());
    }

    #[test]
    fn parses_input_size_from_banner() {
        assert_eq!(
            parse_video_size("  Stream #0:0: Video: rawvideo (UYVY / 0x59565955), uyvy422, 720x576, 25 fps, 25 tbr"),
            Some((720, 576))
        );
        assert_eq!(parse_video_size("  Stream #0:0: Video: wrapped_avframe, yuv420p, 1280x720 [SAR 1:1 DAR 16:9], 30 fps"), Some((1280, 720)));
        assert_eq!(parse_video_size("  Stream #0:1: Audio: aac, 48000 Hz"), None);
    }

    #[test]
    fn graph_splits_preview_before_overlay() {
        let mut c = cfg(vec![text(0.1, 0.1)]);
        c.preview_dir = Some("/tmp/pv".into());
        let ch = ChannelConfig { id: 1, source: Source::TestPattern, size: None, fps: 30, deinterlace: Deinterlace::On, detected_size: None, detected_format: None };
        let a = build_args(&c, &ch, Encoder::X264, Path::new("/s/CH1_p0_%05d.mp4"), None);
        let fc = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        let split = fc.find("split=2[rec0][pv]").unwrap();
        let draw = fc.find("drawtext=").unwrap();
        assert!(fc.starts_with("[0:v]bwdif=mode=send_frame[d];[d]split=2"), "{fc}");
        assert!(split < draw, "preview must be taken before the overlay: {fc}");
        assert!(a.windows(2).any(|w| w[0] == "-map" && w[1] == "[t0]"));
        assert!(a.windows(2).any(|w| w[0] == "-map" && w[1] == "[pvo]"));
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    const AVF: &str = "[in#0 @ 0xc63020000] Selected framerate (60.000000) is not supported by the device.\n[in#0 @ 0xc63020000] Supported modes:\n[in#0 @ 0xc63020000]   640x480@[15.000000 30.000000]fps\n[in#0 @ 0xc63020000]   1280x720@[15.000000 30.000000]fps\n[in#0 @ 0xc63020000]   1920x1080@[15.000000 30.000000]fps\n";

    #[test]
    fn avfoundation_modes_and_fallback() {
        let m = parse_avfoundation_modes(AVF);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1], Mode { w: 1280, h: 720, fps: vec![15.0, 30.0] });
        assert_eq!(unsupported(AVF), Some(Unsupported::Framerate));
        assert_eq!(best_fps(&m, Some((1280, 720)), 60, &[]), Some(30));
        assert_eq!(best_fps(&m, None, 24, &[]), Some(15));
        assert_eq!(best_fps(&m, None, 10, &[]), Some(15));
        // listed but refused by the device (seen on a MacBook camera): skip it
        assert_eq!(best_fps(&m, Some((1280, 720)), 15, &[15]), Some(30));
    }

    #[test]
    fn default_mode_avoids_heavy_portrait_and_oversize() {
        let m = vec![
            Mode { w: 1080, h: 1920, fps: vec![15.0, 30.0] },
            Mode { w: 1920, h: 1080, fps: vec![15.0, 30.0] },
            Mode { w: 1280, h: 720, fps: vec![15.0, 30.0] },
            Mode { w: 640, h: 480, fps: vec![15.0, 30.0] },
        ];
        assert_eq!(pick_default_mode(&m, 30), Some(((1280, 720), 30)));
        assert_eq!(pick_default_mode(&m, 15), Some(((1280, 720), 15)));
        let hd_only = vec![Mode { w: 1920, h: 1080, fps: vec![25.0, 30.0] }];
        assert_eq!(pick_default_mode(&hd_only, 60), Some(((1920, 1080), 30)));
        assert_eq!(pick_default_mode(&[], 30), None);
    }

    #[test]
    fn dshow_ranges_become_common_rates() {
        let t = "[dshow @ 0x1]   pixel_format=yuyv422  min s=640x480 fps=5 max s=640x480 fps=30\n[dshow @ 0x1]   vcodec=mjpeg  min s=640x480 fps=5 max s=1920x1080 fps=30\n";
        let m = parse_dshow_modes(t);
        let hd = m.iter().find(|x| (x.w, x.h) == (1920, 1080)).unwrap();
        assert_eq!(hd.fps, vec![5.0, 10.0, 15.0, 24.0, 25.0, 30.0]);
        assert!(m.iter().any(|x| (x.w, x.h) == (640, 480)));
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;

    #[test]
    fn preview_only_has_no_encoder_or_segments() {
        let cfg = RecordConfig { preview_only: true, preview_dir: Some("/tmp/pv".into()), ..Default::default() };
        let ch = ChannelConfig { id: 2, source: Source::TestPattern, size: None, fps: 30, deinterlace: Deinterlace::On, detected_size: None, detected_format: None };
        let a = build_args(&cfg, &ch, Encoder::X264, Path::new("/s/x_%05d.mp4"), None);
        assert!(!a.iter().any(|x| x == "segment" || x == "-segment_time" || x == "libx264"));
        assert_eq!(a.last().unwrap(), "/tmp/pv/CH2.jpg");
        assert!(a.iter().any(|x| x.contains("bwdif=mode=send_frame[d];[d]scale=w='min(640,iw)'")));
    }

    #[test]
    fn camera_native_preview_skips_cpu_conversion() {
        let cfg = RecordConfig { preview_only: true, preview_dir: Some("/tmp/pv".into()), ..Default::default() };
        let ch = ChannelConfig { id: 1, source: Source::TestPattern, size: None, fps: 30, deinterlace: Deinterlace::Off, detected_size: None, detected_format: None };
        let spec = PreviewSpec { port: 99, kind: StreamKind::Raw { w: 1280, h: 720, fmt: crate::preview::RawFmt::Uyvy } };
        let a = build_args(&cfg, &ch, Encoder::X264, Path::new("/s/x_%05d.mp4"), Some(&spec));
        assert!(a.windows(2).any(|w| w[0] == "-f" && w[1] == "rawvideo"));
        assert!(a.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "uyvy422"));
        let fc = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(fc, "[0:v]format=uyvy422[pvo]");
    }

    #[test]
    fn pix_fmt_from_banner() {
        assert_eq!(parse_video_pix_fmt("  Stream #0:0: Video: rawvideo (UYVY / 0x59565955), uyvy422, 1280x720, 30 tbr").as_deref(), Some("uyvy422"));
        assert_eq!(parse_video_pix_fmt("  Stream #0:0: Video: wrapped_avframe, yuv420p, 1280x720 [SAR 1:1 DAR 16:9], 30 fps").as_deref(), Some("yuv420p"));
        assert_eq!(parse_video_pix_fmt("  Stream #0:1: Audio: aac"), None);
    }

    #[test]
    fn raw_preview_goes_to_the_relay_socket() {
        let cfg = RecordConfig { preview_only: true, preview_dir: Some("/tmp/pv".into()), ..Default::default() };
        let ch = ChannelConfig { id: 1, source: Source::TestPattern, size: None, fps: 60, deinterlace: Deinterlace::Off, detected_size: None, detected_format: None };
        let a = build_args(&cfg, &ch, Encoder::X264, Path::new("/s/x_%05d.mp4"), Some(&PreviewSpec { port: 4321, kind: StreamKind::Y4m }));
        assert_eq!(a.last().unwrap(), "tcp://127.0.0.1:4321");
        assert!(a.windows(2).any(|w| w[0] == "-f" && w[1] == "yuv4mpegpipe"));
        assert!(a.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "yuv420p"));
        // 60 fps sources are halved for the preview instead of being sent in full
        assert!(a.iter().any(|x| x.contains("fps=30,scale=")));
    }
}
