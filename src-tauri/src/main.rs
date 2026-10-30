#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use recorder_core::devices::{self, VideoDevice};
use recorder_core::probe::{self, Encoder};
use recorder_core::overlay::OverlayConfig;
use recorder_core::preview::PreviewHub;
use recorder_core::storage::StorageConfig;
use recorder_core::{ChannelConfig, ChannelState, Deinterlace, ChannelStatus, RecordConfig, Recorder, Source, SplitLayout};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tokio::sync::Mutex;

/// What the UI edits and we persist. Kept separate from `RecordConfig` so the on-disk format
/// stays stable when core internals change.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    root: String,
    segment_minutes: u64,
    split_layout: SplitLayout,
    encoder: Option<Encoder>,
    bitrate: String,
    preview: bool,
    /// Widest preview in pixels: 640 (light), 960 or 1280 (full size for a 720p camera).
    #[serde(default = "default_preview_width")]
    preview_width: u32,
    #[serde(default)]
    overlay: OverlayConfig,
    #[serde(default)]
    storage: StorageConfig,
    channels: Vec<ChannelSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelSetting {
    enabled: bool,
    /// `None` = synthetic test pattern (no hardware needed).
    device: Option<String>,
    size: Option<String>,
    fps: u32,
    #[serde(default)]
    deinterlace: Deinterlace,
}

fn default_preview_width() -> u32 {
    recorder_core::preview::DEFAULT_MAX_WIDTH
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            root: dirs_home().join("XspectorRecordings").to_string_lossy().into_owned(),
            segment_minutes: 30,
            split_layout: SplitLayout::NewFolder,
            encoder: None,
            bitrate: "4M".into(),
            preview: true,
            preview_width: default_preview_width(),
            overlay: OverlayConfig::default(),
            storage: StorageConfig::default(),
            channels: vec![ChannelSetting {
                enabled: true,
                device: None,
                size: None,
                fps: 30,
                deinterlace: Deinterlace::Off,
            }],
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A bundled sidecar next to the executable wins. GUI apps on macOS do not inherit the shell
/// PATH, so Homebrew locations are checked explicitly before falling back to PATH.
fn resolve_ffmpeg() -> PathBuf {
    let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let mut candidates: Vec<PathBuf> = vec![];
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            candidates.push(dir.join(exe));
        }
    }
    if let Some(p) = std::env::var_os("FFMPEG") {
        candidates.insert(0, PathBuf::from(p));
    }
    if cfg!(target_os = "macos") {
        // ffmpeg-full has drawtext (text overlay); the plain formula does not.
        candidates.push("/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg".into());
        candidates.push("/opt/homebrew/bin/ffmpeg".into());
        candidates.push("/usr/local/bin/ffmpeg".into());
    }
    candidates.into_iter().find(|p| p.exists()).unwrap_or_else(|| PathBuf::from(exe))
}

#[derive(Default)]
struct AppState {
    recorder: Mutex<Option<Recorder>>,
    /// Live monitor: shows the camera before/between recordings. Never runs at the same time as
    /// the recorder, because a capture device can only be opened by one process.
    monitor: Mutex<Option<Recorder>>,
    last_settings: Mutex<Option<Settings>>,
    /// Why the last recording ended by itself (e.g. disk full); shown until the next start.
    ended_notice: Mutex<Vec<String>>,
    /// Preview frames from every FFmpeg go through this hub to the loopback MJPEG server.
    hub: std::sync::Arc<PreviewHub>,
    /// Running preview forwarders, by subscription token.
    watchers: Mutex<std::collections::HashMap<u64, tauri::async_runtime::JoinHandle<()>>>,
    next_watch: std::sync::atomic::AtomicU64,
}

/// Previews are throwaway and tiny, so they live in the OS temp dir, not the recording folder.
fn preview_dir() -> PathBuf {
    std::env::temp_dir().join("xspector-record-preview")
}

#[derive(Serialize)]
struct StatusDto {
    /// false = live monitor only (nothing is being recorded)
    recording: bool,
    started_ms: i64,
    encoder: Encoder,
    /// overlay can be edited while recording
    layer_active: bool,
    warnings: Vec<String>,
    values: std::collections::BTreeMap<String, String>,
    channels: Vec<ChannelStatus>,
}

fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

#[tauri::command]
fn load_settings(app: tauri::AppHandle) -> Result<Settings, String> {
    let p = settings_path(&app)?;
    Ok(std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(&app)?, json).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_devices() -> Result<Vec<VideoDevice>, String> {
    devices::list_video(&resolve_ffmpeg()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn device_modes(name: String) -> Result<Vec<recorder_core::ffmpeg::Mode>, String> {
    devices::list_modes(&resolve_ffmpeg(), &name).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn usable_encoders() -> Vec<Encoder> {
    probe::usable(&resolve_ffmpeg()).await
}

fn build_cfg(settings: &Settings, preview_only: bool, hub: std::sync::Arc<PreviewHub>) -> RecordConfig {
    let channels: Vec<ChannelConfig> = settings
        .channels
        .iter()
        .enumerate()
        .filter(|(_, c)| c.enabled)
        .map(|(i, c)| ChannelConfig {
            id: i as u32 + 1,
            source: c.device.clone().map_or(Source::TestPattern, Source::Device),
            size: c.size.clone().filter(|s| !s.is_empty()),
            fps: c.fps,
            deinterlace: c.deinterlace,
            detected_size: None,
            detected_format: None,
        })
        .collect();
    RecordConfig {
        ffmpeg_path: resolve_ffmpeg(),
        root: settings.root.clone().into(),
        segment_secs: settings.segment_minutes * 60,
        split_layout: settings.split_layout,
        encoder: settings.encoder,
        bitrate: settings.bitrate.clone(),
        preview_dir: (settings.preview || preview_only).then(preview_dir),
        overlay: settings.overlay.clone(),
        storage: settings.storage,
        overlay_dir: None,
        preview_only,
        preview_hub: Some(hub),
        preview_max_width: settings.preview_width.clamp(320, 1920),
        channels,
    }
}

async fn stop_monitor(state: &AppState) {
    let m = state.monitor.lock().await.take();
    if let Some(m) = m {
        m.stop().await;
    }
}

/// (Re)start the live monitor from the last known settings, unless recording or preview is off.
async fn start_monitor(state: &AppState, clear: bool) -> Result<(), String> {
    if state.recorder.lock().await.is_some() {
        return Ok(());
    }
    let Some(settings) = state.last_settings.lock().await.clone() else { return Ok(()) };
    stop_monitor(state).await;
    if !settings.preview || !settings.channels.iter().any(|c| c.enabled) {
        return Ok(());
    }
    let dir = preview_dir();
    if clear {
        let _ = tokio::fs::remove_dir_all(&dir).await; // no stale frame from the previous source
    }
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let rec = Recorder::start(build_cfg(&settings, true, state.hub.clone())).await.map_err(|e| {
        if std::env::var_os("XSPECTOR_DEBUG").is_some() {
            eprintln!("[monitor] start failed: {e}");
        }
        e.to_string()
    })?;
    *state.monitor.lock().await = Some(rec);
    if std::env::var_os("XSPECTOR_DEBUG").is_some() {
        eprintln!("[monitor] started");
    }
    Ok(())
}

/// Called by the UI whenever the channel/overlay/preview settings change (and at startup).
#[tauri::command]
async fn set_monitor(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    *state.last_settings.lock().await = Some(settings);
    start_monitor(&state, true).await
}

#[tauri::command]
async fn start_recording(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    if state.recorder.lock().await.is_some() {
        return Err("already recording".into());
    }
    *state.last_settings.lock().await = Some(settings.clone());
    state.ended_notice.lock().await.clear();
    // The monitor already saw each device's real frame size; remember it, then free the device.
    let sizes = match state.monitor.lock().await.as_ref() {
        Some(m) => m.input_sizes(),
        None => Default::default(),
    };
    stop_monitor(&state).await;
    if settings.preview {
        tokio::fs::create_dir_all(preview_dir()).await.map_err(|e| e.to_string())?;
    }
    let mut cfg = build_cfg(&settings, false, state.hub.clone());
    for ch in &mut cfg.channels {
        ch.detected_size = sizes.get(&ch.id).copied();
    }
    match Recorder::start(cfg).await {
        Ok(rec) => {
            *state.recorder.lock().await = Some(rec);
            Ok(())
        }
        Err(e) => {
            let _ = start_monitor(&state, false).await;
            Err(e.to_string())
        }
    }
}

/// Push overlay edits into a running recording (layer mode).
#[tauri::command]
async fn update_overlay(state: State<'_, AppState>, overlay: OverlayConfig) -> Result<(), String> {
    if let Some(r) = state.recorder.lock().await.as_ref() {
        r.update_overlay(overlay);
    }
    Ok(())
}

#[tauri::command]
async fn stop_recording(state: State<'_, AppState>) -> Result<(), String> {
    // Take it out first so the lock is not held while FFmpeg finalizes files.
    let rec = state.recorder.lock().await.take();
    if let Some(r) = rec {
        r.stop().await;
    }
    start_monitor(&state, false).await
}

#[tauri::command]
async fn status(state: State<'_, AppState>) -> Result<Option<StatusDto>, String> {
    let dto = |r: &Recorder, recording: bool, extra: Vec<String>| StatusDto {
        recording,
        started_ms: r.session_start.timestamp_millis(),
        encoder: r.encoder,
        layer_active: r.layer_active,
        warnings: r.warnings.iter().cloned().chain(r.notices()).chain(extra).collect(),
        values: r.live_values(),
        channels: r.status(),
    };
    // A recording that ended by itself (disk full -> "stop"): finish up, remember why, go back to
    // the live monitor so the UI returns to idle instead of showing a dead recording.
    let ended = {
        let g = state.recorder.lock().await;
        g.as_ref().is_some_and(|r| r.status().iter().all(|c| c.state == ChannelState::Stopped))
    };
    if ended {
        if let Some(r) = state.recorder.lock().await.take() {
            *state.ended_notice.lock().await = r.notices();
            r.stop().await;
        }
        let _ = start_monitor(&state, false).await;
    }
    if let Some(r) = state.recorder.lock().await.as_ref() {
        return Ok(Some(dto(r, true, vec![])));
    }
    let extra = state.ended_notice.lock().await.clone();
    Ok(state.monitor.lock().await.as_ref().map(|r| dto(r, false, extra)))
}

/// Latest JPEG for a channel as raw bytes (empty until the first frame is written).
/// Stream the live preview of a channel to the UI: one raw JPEG per message on `on_frame`.
/// Goes through Tauri's IPC (no network port is opened). Returns a token for [`unwatch_preview`].
#[tauri::command]
async fn watch_preview(
    state: State<'_, AppState>,
    id: u32,
    on_frame: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> Result<u64, String> {
    use tauri::ipc::InvokeResponseBody::Raw;
    let (mut rx, latest) = state.hub.subscribe(id);
    let task = tauri::async_runtime::spawn(async move {
        if let Some(f) = latest {
            let _ = on_frame.send(Raw((*f).clone()));
        }
        loop {
            match rx.recv().await {
                Ok(f) => {
                    if on_frame.send(Raw((*f).clone())).is_err() {
                        break; // the page is gone
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue, // newest wins
                Err(_) => break,
            }
        }
    });
    let token = state.next_watch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.watchers.lock().await.insert(token, task);
    Ok(token)
}

/// Diagnostics from the UI, printed only when XSPECTOR_DEBUG is set (e.g. to measure the real
/// painted frame rate of the preview).
#[tauri::command]
fn debug_log(msg: String) {
    if std::env::var_os("XSPECTOR_DEBUG").is_some() {
        eprintln!("[ui] {msg}");
    }
}

#[tauri::command]
fn debug_enabled() -> bool {
    std::env::var_os("XSPECTOR_DEBUG").is_some()
}

#[tauri::command]
async fn unwatch_preview(state: State<'_, AppState>, token: u64) -> Result<(), String> {
    if let Some(t) = state.watchers.lock().await.remove(&token) {
        t.abort();
    }
    Ok(())
}

#[tauri::command]
async fn preview(id: u32) -> tauri::ipc::Response {
    let bytes = tokio::fs::read(preview_dir().join(format!("CH{id}.jpg"))).await.unwrap_or_default();
    tauri::ipc::Response::new(bytes)
}

/// Bytes of an overlay picture for the editor. Limited to image files.
#[tauri::command]
async fn read_image(path: String) -> Result<tauri::ipc::Response, String> {
    let p = PathBuf::from(&path);
    let ok = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"));
    if !ok {
        return Err("not an image file".into());
    }
    let bytes = tokio::fs::read(&p).await.map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn disk_free(path: String) -> Option<u64> {
    // Works before the first recording too: the core walks up to the nearest existing folder.
    tokio::task::spawn_blocking(move || recorder_core::storage::free_space(std::path::Path::new(&path)))
        .await
        .ok()
        .flatten()
}

/// Pop a dock panel out into its own OS window (like OBS docks / xspector-portable).
/// The window loads the same UI in single-panel mode; closing it tells the main window to re-dock.
#[tauri::command]
fn open_panel_window(app: tauri::AppHandle, id: String, title: String, width: f64, height: f64) -> Result<(), String> {
    let label = format!("panel-{id}");
    if let Some(w) = app.get_webview_window(&label) {
        return w.set_focus().map_err(|e| e.to_string());
    }
    let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(format!("index.html?panel={id}").into()))
        .title(format!("Xspector Record · {title}"))
        .inner_size(width, height)
        .min_inner_size(240.0, 160.0)
        .build()
        .map_err(|e| e.to_string())?;
    let handle = app.clone();
    win.on_window_event(move |e| {
        if matches!(e, WindowEvent::Destroyed) {
            let _ = handle.emit("panel-closed", id.clone());
        }
    });
    Ok(())
}

#[tauri::command]
fn close_panel_window(app: tauri::AppHandle, id: String) {
    if let Some(w) = app.get_webview_window(&format!("panel-{id}")) {
        let _ = w.destroy();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            load_settings,
            save_settings,
            list_devices,
            device_modes,
            usable_encoders,
            start_recording,
            stop_recording,
            set_monitor,
            update_overlay,
            status,
            preview,
            watch_preview,
            unwatch_preview,
            debug_log,
            debug_enabled,
            read_image,
            disk_free,
            open_panel_window,
            close_panel_window
        ])
        .build(tauri::generate_context!())
        .expect("build app")
        .run(|app, event| {
            // Quit must finalize open files, otherwise the last segment depends on crash recovery.
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                let state = app.state::<AppState>();
                let (rec, mon) = tauri::async_runtime::block_on(async {
                    (state.recorder.lock().await.take(), state.monitor.lock().await.take())
                });
                if rec.is_some() || mon.is_some() {
                    api.prevent_exit();
                    let handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(r) = rec {
                            r.stop().await;
                        }
                        if let Some(m) = mon {
                            m.stop().await;
                        }
                        handle.exit(0);
                    });
                }
            }
        });
}
