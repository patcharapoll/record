use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::{RecordConfig, Source};
use crate::devices;
use crate::ffmpeg;
use crate::layout;
use crate::mixer::{self, MixerParams, MixerStats};
use crate::overlay::{self, DataBus, OverlayConfig, OverlayMode};
use crate::preview::{self, RawFmt, StreamKind};
use crate::probe::{self, Encoder};
use crate::storage;
use crate::{Error, Result};

/// No frame progress for this long means the device is wedged (unplugged, driver hang).
/// No frames for this long = "no signal". The device stays open and waits for the signal to return.
const STALL_AFTER: Duration = Duration::from_secs(10);
/// A device that stays silent this long may be hung rather than unplugged: reopen it once.
const NO_SIGNAL_REOPEN: Duration = Duration::from_secs(300);
/// A device that never delivered a single frame is more likely a failed open than a missing signal.
const NEVER_STARTED_REOPEN: Duration = Duration::from_secs(20);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
static INSTANCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelState {
    Starting,
    Recording,
    Restarting,
    /// The device is open but delivers no frames (nothing plugged into the capture card).
    NoSignal,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelStatus {
    pub id: u32,
    pub state: ChannelState,
    pub frames: u64,
    pub dropped: u64,
    /// Frames repeated because the camera delivered late; each one is a visible stutter.
    pub duplicated: u64,
    pub speed: f64,
    pub segments: u32,
    /// Frame rate actually requested from the device (may differ from the setting if the camera cannot do it).
    pub fps: u32,
    pub restarts: u32,
    /// Percent of one core used by this channel's FFmpeg process.
    pub cpu_percent: f32,
    pub rss_mb: f32,
    pub last_file: Option<String>,
    pub last_error: Option<String>,
}

impl ChannelStatus {
    fn new(id: u32) -> Self {
        Self {
            id,
            state: ChannelState::Starting,
            frames: 0,
            dropped: 0,
            duplicated: 0,
            speed: 0.0,
            segments: 0,
            fps: 0,
            restarts: 0,
            cpu_percent: 0.0,
            rss_mb: 0.0,
            last_file: None,
            last_error: None,
        }
    }
}

struct Ctx {
    cfg: RecordConfig,
    enc: Encoder,
    session_start: DateTime<Local>,
    staging: PathBuf,
    status: watch::Sender<Vec<ChannelStatus>>,
    bus: DataBus,
    overlay_rx: watch::Receiver<OverlayConfig>,
    /// Frame size reported by each channel's FFmpeg banner (used by the live monitor).
    sizes: Mutex<std::collections::HashMap<u32, (u32, u32)>>,
    overrides: Mutex<std::collections::HashMap<u32, Override>>,
}

/// Aborts the task when dropped, so a layer feeder never outlives its FFmpeg process.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Ctx {
    fn update(&self, idx: usize, f: impl FnOnce(&mut ChannelStatus)) {
        self.status.send_modify(|v| f(&mut v[idx]));
    }
}

pub struct Recorder {
    pub session_start: DateTime<Local>,
    /// True for the live monitor (preview only, nothing recorded).
    pub preview_only: bool,
    /// The overlay is a live layer: it can be edited while recording.
    pub layer_active: bool,
    pub encoder: Encoder,
    /// Things worth telling the user, e.g. the overlay was skipped.
    pub warnings: Vec<String>,
    bus: DataBus,
    overlay_tx: watch::Sender<OverlayConfig>,
    overlay_dir: Option<PathBuf>,
    background: Vec<JoinHandle<()>>,
    /// Runtime messages (disk space actions etc.).
    notices: Arc<Mutex<Vec<String>>>,
    stop: Arc<watch::Sender<bool>>,
    status: watch::Receiver<Vec<ChannelStatus>>,
    tasks: Vec<JoinHandle<()>>,
    ctx: Arc<Ctx>,
}

impl Recorder {
    pub async fn start(mut cfg: RecordConfig) -> Result<Self> {
        cfg.validate()?;
        Command::new(&cfg.ffmpeg_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|e| Error::Ffmpeg(format!("{}: {e}", cfg.ffmpeg_path.display())))?;

        let preview_only = cfg.preview_only;
        let enc = match (cfg.encoder, preview_only) {
            (Some(e), _) => e,
            (None, true) => Encoder::X264, // never used: the monitor does not encode video
            (None, false) => probe::best(&cfg.ffmpeg_path).await?,
        };
        let staging = cfg.root.join(".recording");
        if !preview_only {
            tokio::fs::create_dir_all(&staging).await?;
            recover_leftovers(&cfg, &staging).await;
        }

        // Overlay: needs FFmpeg's drawtext (libfreetype). Without it, record without overlay.
        let mut warnings = vec![];
        let bus = DataBus::default();
        let mut background = vec![];
        let mut overlay_dir = None;
        // Live data (the monitor needs it too, for the editor); text burn-in only while recording.
        if let Some(h) = overlay::start_source(&cfg.overlay.source, bus.clone()) {
            background.push(h);
        }
        let has_text = cfg.overlay.text_items().next().is_some();
        let has_image = cfg.overlay.items.iter().any(|i| i.enabled && i.kind == overlay::ItemKind::Image);
        // The Fixed mode relies on FFmpeg for everything it draws; if this build lacks a piece, use
        // the live layer (drawn by the app) instead of letting FFmpeg fail on every start.
        if cfg.overlay.enabled && cfg.overlay.mode == OverlayMode::Burn && !preview_only && has_image {
            let ext_ok = cfg.overlay.items.iter().filter(|i| i.enabled && i.kind == overlay::ItemKind::Image).all(|i| {
                i.image.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg")
                })
            });
            if !ext_ok || !probe::has_decoder(&cfg.ffmpeg_path, "png").await || !probe::has_filter(&cfg.ffmpeg_path, "movie").await {
                cfg.overlay.mode = OverlayMode::Layer;
                warnings.push("Fixed overlay cannot load these pictures with this FFmpeg; using the live layer.".to_string());
            }
        }
        if cfg.overlay.enabled && cfg.overlay.mode == OverlayMode::Burn && !preview_only && has_text {
            // Fixed text needs FFmpeg's drawtext (libfreetype + harfbuzz), which the shipped
            // FFmpeg does not have. Fall back to the live layer instead of dropping the text.
            if !probe::has_filter(&cfg.ffmpeg_path, "drawtext").await {
                cfg.overlay.mode = OverlayMode::Layer;
                warnings.push("Fixed overlay needs FFmpeg's drawtext, which this FFmpeg lacks; using the live layer.".to_string());
            } else {
                let n = INSTANCE.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir().join(format!("xspector-record-overlay-{}-{n}", std::process::id()));
                let _ = tokio::fs::remove_dir_all(&dir).await;
                tokio::fs::create_dir_all(&dir).await?;
                let ids = cfg.channels.iter().map(|c| c.id).collect();
                let texts = cfg.overlay.text_items().map(|(i, it)| (i, it.clone())).collect();
                background.push(overlay::start_writer(dir.clone(), texts, ids, bus.clone()).await);
                cfg.overlay_dir = Some(dir.clone());
                overlay_dir = Some(dir);
            }
        }

        let session_start = Local::now();
        let (status_tx, status_rx) = watch::channel(
            cfg.channels.iter().map(|c| ChannelStatus::new(c.id)).collect::<Vec<_>>(),
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let stop_tx = Arc::new(stop_tx);
        let notices = Arc::new(Mutex::new(Vec::<String>::new()));
        if !preview_only {
            background.push(storage::spawn_monitor(cfg.storage, cfg.root.clone(), notices.clone(), stop_tx.clone()));
        }
        let n = cfg.channels.len();
        let (overlay_tx, overlay_rx) = watch::channel(cfg.overlay.clone());
        let ctx = Arc::new(Ctx { cfg, enc, session_start, staging, status: status_tx, bus: bus.clone(), overlay_rx, sizes: Default::default(), overrides: Default::default() });
        let tasks = (0..n)
            .map(|idx| tokio::spawn(run_channel(ctx.clone(), idx, stop_rx.clone())))
            .collect();
        let layer_active = !preview_only && ctx.cfg.overlay.enabled && ctx.cfg.overlay.mode == OverlayMode::Layer;
        Ok(Self { session_start, preview_only, layer_active, encoder: enc, warnings, bus, overlay_tx, overlay_dir, background, notices, stop: stop_tx, status: status_rx, tasks, ctx })
    }

    pub fn status(&self) -> Vec<ChannelStatus> {
        self.status.borrow().clone()
    }

    /// Change the overlay while recording. Works in `Layer` mode when the overlay was enabled at
    /// start (the layer stream exists from then on); `Burn` mode is fixed once started.
    pub fn update_overlay(&self, overlay: OverlayConfig) {
        let _ = self.overlay_tx.send(overlay);
    }

    /// Frame size each channel's device delivers, as seen in FFmpeg's banner.
    pub fn input_sizes(&self) -> std::collections::HashMap<u32, (u32, u32)> {
        self.ctx.sizes.lock().unwrap().clone()
    }

    /// Messages raised while running, e.g. old recordings were deleted to free space.
    pub fn notices(&self) -> Vec<String> {
        self.notices.lock().unwrap().clone()
    }

    /// Latest overlay data (sensor values).
    pub fn live_values(&self) -> std::collections::BTreeMap<String, String> {
        self.bus.snapshot()
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<ChannelStatus>> {
        self.status.clone()
    }

    /// Ask every FFmpeg to finish its current file cleanly, then wait for them.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        for t in self.tasks {
            let _ = t.await;
        }
        for b in &self.background {
            b.abort();
        }
        if let Some(dir) = &self.overlay_dir {
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
    }
}

/// The capture half of the live-overlay pipeline.
struct Capture {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    mixer: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    stats: Arc<MixerStats>,
    err: tokio::task::JoinHandle<()>,
}

struct Outcome {
    stopped: bool,
    reason: String,
    ran: Duration,
    /// The camera rejected the mode and the settings were corrected: start again right away.
    retry_now: bool,
}

/// Per-channel corrections learned from the device (unsupported frame rate / size).
#[derive(Default, Clone)]
struct Override {
    fps: Option<u32>,
    drop_size: bool,
    /// Frame rates the device refused even though its mode list shows them.
    rejected_fps: Vec<u32>,
    /// Mode picked automatically when the user left the size on Auto (size, fps).
    auto: Option<((u32, u32), u32)>,
    auto_done: bool,
}

async fn run_channel(ctx: Arc<Ctx>, idx: usize, mut stop: watch::Receiver<bool>) {
    let mut backoff = Duration::from_secs(1);
    let mut proc_no = 0u32;
    while !*stop.borrow() {
        ctx.update(idx, |s| s.state = ChannelState::Starting);
        let out = run_once(&ctx, idx, proc_no, &mut stop).await;
        proc_no += 1;
        if out.stopped {
            break;
        }
        if out.ran > Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        ctx.update(idx, |s| {
            s.state = ChannelState::Restarting;
            s.restarts += 1;
            s.last_error = Some(out.reason.clone());
            s.cpu_percent = 0.0;
        });
        if out.retry_now {
            continue; // settings were corrected from what the camera reported; no need to wait
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    ctx.update(idx, |s| {
        s.state = ChannelState::Stopped;
        s.cpu_percent = 0.0;
    });
}

async fn run_once(
    ctx: &Arc<Ctx>,
    idx: usize,
    proc_no: u32,
    stop: &mut watch::Receiver<bool>,
) -> Outcome {
    let mut eff = ctx.cfg.channels[idx].clone();
    if let Some(o) = ctx.overrides.lock().unwrap().get(&eff.id) {
        if let Some(f) = o.fps {
            eff.fps = f;
        }
        if o.drop_size {
            eff.size = None;
        }
    }
    // Size left on "Auto" for a real device: choose a sensible mode instead of the camera's default.
    if let (Source::Device(name), None) = (&eff.source, &eff.size) {
        let (done, drop) = ctx.overrides.lock().unwrap().get(&eff.id).map_or((false, false), |o| (o.auto_done, o.drop_size));
        if !done && !drop {
            let modes = devices::list_modes(&ctx.cfg.ffmpeg_path, name).await.unwrap_or_default();
            let pick = ffmpeg::pick_default_mode(&modes, eff.fps);
            let mut ov = ctx.overrides.lock().unwrap();
            let o = ov.entry(eff.id).or_default();
            o.auto = pick;
            o.auto_done = !modes.is_empty(); // try again later if the listing failed (device not ready)
        }
        if let Some(((w, h), fps)) = ctx.overrides.lock().unwrap().get(&eff.id).and_then(|o| o.auto) {
            if !drop {
                eff.size = Some(format!("{w}x{h}"));
                eff.fps = fps;
            }
        }
    }
    let ch = &eff;
    ctx.update(idx, |s| s.fps = ch.fps);
    let prefix = format!("CH{}_p{}_", ch.id, proc_no);
    let pattern = ctx.staging.join(format!("{prefix}%05d.mp4"));
    let proc_start = Local::now();
    let t0 = Instant::now();
    let fail = |reason: String| Outcome { stopped: false, reason, ran: t0.elapsed(), retry_now: false };

    let progress_ms = Arc::new(AtomicU64::new(0));
    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));

    // Live (editable) overlay: capture -> compositor (this process) -> encoder, as two FFmpegs.
    let ov = &ctx.cfg.overlay;
    let mixed = ov.enabled && ov.mode == OverlayMode::Layer && !ctx.cfg.preview_only;
    let mut capture: Option<Capture> = None;
    let mut _preview_relay: Option<AbortOnDrop> = None;
    let mut preview_spec: Option<ffmpeg::PreviewSpec> = None;
    let args: Vec<String>;
    let mut cap_stdout: Option<tokio::process::ChildStdout> = None;
    if mixed {
        let stats = Arc::new(MixerStats::default());
        let mut cmd = Command::new(&ctx.cfg.ffmpeg_path);
        cmd.args(ffmpeg::build_capture_args(&ctx.cfg, ch))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return fail(format!("spawn capture: {e}")),
        };
        let stdin = child.stdin.take();
        cap_stdout = child.stdout.take();
        let cap_err = child.stderr.take().unwrap();
        let err = {
            let tail = tail.clone();
            tokio::spawn(async move {
                read_records(cap_err, |line| {
                    if !line.trim().is_empty() && !line.contains("frame=") {
                        let mut t = tail.lock().unwrap();
                        if t.len() >= 80 {
                            t.pop_front();
                        }
                        t.push_back(line);
                    }
                    async {}
                })
                .await
            })
        };
        capture = Some(Capture { child, stdin, mixer: None, stats, err });
        args = ffmpeg::build_encoder_args(&ctx.cfg, ch, ctx.enc, &pattern);
    } else {
        // Live preview: FFmpeg sends raw frames to a loopback socket that we relay to the UI, where the
        // GPU converts and draws them. If we know the camera's own pixel format and size, those frames
        // are sent as they are (no conversion or scaling on the CPU).
        if let (Some(hub), true) = (&ctx.cfg.preview_hub, ctx.cfg.preview_dir.is_some()) {
            let mut kind = StreamKind::Y4m;
            if let (Source::Device(name), Some(size)) = (&ch.source, ch.size.as_deref()) {
                let dims = size.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
                let known = ch.detected_format.clone().or_else(|| device_format(name, size, ch.fps));
                let fmt = match known {
                    Some(f) => Some(f),
                    None => {
                        let probed = devices::probe_pix_fmt(&ctx.cfg.ffmpeg_path, name, Some(size), ch.fps).await;
                        if let Some(f) = &probed {
                            remember_device_format(name, size, ch.fps, f);
                        }
                        probed
                    }
                };
                if let (Some((w, h)), Some(fmt)) = (dims, fmt.as_deref().and_then(RawFmt::from_pix_fmt)) {
                    if w <= ctx.cfg.preview_max_width && w % 2 == 0 && h % 2 == 0 {
                        kind = StreamKind::Raw { w, h, fmt };
                    }
                }
            }
            if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                if let Ok(addr) = listener.local_addr() {
                    _preview_relay = Some(AbortOnDrop(preview::relay(listener, ch.id, hub.clone(), kind)));
                    preview_spec = Some(ffmpeg::PreviewSpec { port: addr.port(), kind });
                }
            }
        }
        args = ffmpeg::build_args(&ctx.cfg, ch, ctx.enc, &pattern, preview_spec.as_ref());
    }

    let mut cmd = Command::new(&ctx.cfg.ffmpeg_path);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("spawn: {e}")),
    };
    let pid = child.id();
    let mut stdin = child.stdin.take();
    let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    if let (Some(c), Some(cap_out), Some(enc_in)) = (capture.as_mut(), cap_stdout.take(), stdin.take()) {
        // capture stdout -> compositor -> encoder stdin
        let params = MixerParams {
            channel: ch.id,
            fps: ch.fps,
            hub: ctx.cfg.preview_hub.clone().filter(|_| ctx.cfg.preview_dir.is_some()),
            preview_max_width: ctx.cfg.preview_max_width,
            font_file: ctx.cfg.overlay.font_file.clone(),
            overlay_rx: ctx.overlay_rx.clone(),
            bus: ctx.bus.clone(),
        };
        c.mixer = Some(tokio::spawn(mixer::run(cap_out, enc_in, params, c.stats.clone())));
    }

    // stdout: one CSV line per finished segment -> move it to its final place.
    let finalized = Arc::new(Mutex::new(HashSet::<String>::new()));
    let seg_task = {
        let (ctx, finalized) = (ctx.clone(), finalized.clone());
        tokio::spawn(async move {
            read_records(stdout, |line| {
                let (ctx, finalized) = (ctx.clone(), finalized.clone());
                async move {
                    if let Some((name, start)) = ffmpeg::parse_segment_line(&line) {
                        if finalize(&ctx, idx, &name, proc_start, start).await {
                            finalized.lock().unwrap().insert(name);
                        }
                    }
                }
            })
            .await
        })
    };

    // stderr: progress stats (1 Hz) plus the last few warnings for error reporting.
    let err_task = {
        let (ctx, progress_ms, tail) = (ctx.clone(), progress_ms.clone(), tail.clone());
        tokio::spawn(async move {
            let mut last_frame = 0;
            read_records(stderr, |line| {
                if let Some(st) = ffmpeg::parse_stats(&line) {
                    let advanced = st.frame > last_frame;
                    if advanced {
                        last_frame = st.frame;
                        progress_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
                    }
                    ctx.update(idx, |s| {
                        // With the compositor the encoder keeps counting duplicated frames while the
                        // camera is silent, so only the tick (fresh camera frames) leaves NoSignal.
                        if s.state == ChannelState::Starting || (!mixed && advanced) {
                            s.state = ChannelState::Recording;
                        }
                        // Working again: forget old failures (but keep notes like "using 30 fps").
                        if s.last_error.as_deref().is_some_and(|e| e.starts_with("ffmpeg exited") || e.starts_with("stalled") || e.starts_with("spawn")) {
                            s.last_error = None;
                        }
                        s.frames = st.frame;
                        if !mixed {
                            s.dropped = st.drop;
                            s.duplicated = st.dup;
                        }
                        s.speed = st.speed;
                    });
                } else if let Some(size) = ffmpeg::parse_video_size(&line).filter(|_| ctx.cfg.preview_only) {
                    ctx.sizes.lock().unwrap().entry(ctx.cfg.channels[idx].id).or_insert(size);
                } else if !line.trim().is_empty() && !(ctx.cfg.preview_only && is_banner(&line)) {
                    let mut t = tail.lock().unwrap();
                    if t.len() >= 80 {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
                async {}
            })
            .await
        })
    };

    let mut sys = System::new();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut stopped = false;
    let mut cam_seen = (0u64, Instant::now());
    let mut reason = loop {
        tokio::select! {
            status = child.wait() => break format!("ffmpeg exited: {status:?}"),
            status = async {
                match capture.as_mut() {
                    Some(c) => c.child.wait().await,
                    None => std::future::pending().await,
                }
            } => break format!("capture exited: {status:?}"),
            _ = stop.changed() => {
                if *stop.borrow() {
                    stopped = true;
                    // "q" is FFmpeg's graceful quit: it finalizes the open segment. With the live
                    // overlay the capture side is stopped first; the compositor then closes the
                    // encoder's input and the encoder finalizes by itself.
                    if ctx.cfg.preview_only {
                        // Nothing to finalize: free the camera right away (Start waits for it).
                        let _ = child.kill().await;
                    } else {
                        let first = match capture.as_mut() {
                            Some(c) => c.stdin.take(),
                            None => stdin.take(),
                        };
                        if let Some(mut si) = first {
                            let _ = si.write_all(b"q").await;
                        }
                        if tokio::time::timeout(Duration::from_secs(15), child.wait()).await.is_err() {
                            let _ = child.kill().await;
                        }
                    }
                    break "stopped".to_string();
                }
            }
            _ = tick.tick() => {
                if let Some(pid) = pid {
                    let pid = Pid::from_u32(pid);
                    sys.refresh_processes_specifics(
                        ProcessesToUpdate::Some(&[pid]),
                        true,
                        ProcessRefreshKind::nothing().with_cpu().with_memory(),
                    );
                    if let Some(p) = sys.process(pid) {
                        let (cpu, rss) = (p.cpu_usage(), p.memory() as f32 / 1_048_576.0);
                        ctx.update(idx, |s| { s.cpu_percent = cpu; s.rss_mb = rss; });
                    }
                }
                if let Some(c) = &capture {
                    // frame counters come from the compositor; a stalled camera is judged by it
                    let (fresh, dup, dropped) = (c.stats.fresh.load(Ordering::Relaxed), c.stats.dup.load(Ordering::Relaxed), c.stats.dropped.load(Ordering::Relaxed));
                    ctx.update(idx, |s| { s.duplicated = dup; s.dropped = dropped; });
                    if fresh != cam_seen.0 {
                        cam_seen = (fresh, Instant::now());
                    }
                    let silent = cam_seen.1.elapsed();
                    let limit = if cam_seen.0 == 0 { NEVER_STARTED_REOPEN } else { NO_SIGNAL_REOPEN };
                    if silent > limit {
                        let _ = child.kill().await;
                        break format!("stalled: no camera frames for {}s", silent.as_secs());
                    }
                    let no_signal = silent > STALL_AFTER;
                    ctx.update(idx, |s| match (no_signal, s.state) {
                        (true, ChannelState::Recording | ChannelState::Starting) => s.state = ChannelState::NoSignal,
                        (false, ChannelState::NoSignal) => s.state = ChannelState::Recording,
                        _ => {}
                    });
                } else {
                    let since = t0.elapsed().saturating_sub(Duration::from_millis(progress_ms.load(Ordering::Relaxed)));
                    let limit = if progress_ms.load(Ordering::Relaxed) == 0 { NEVER_STARTED_REOPEN } else { NO_SIGNAL_REOPEN };
                    if since > limit {
                        let _ = child.kill().await;
                        break format!("stalled: no frames for {}s", since.as_secs());
                    }
                    let no_signal = since > STALL_AFTER;
                    ctx.update(idx, |s| match (no_signal, s.state) {
                        (true, ChannelState::Recording | ChannelState::Starting) => s.state = ChannelState::NoSignal,
                        (false, ChannelState::NoSignal) => s.state = ChannelState::Recording,
                        _ => {}
                    });
                }
            }
        }
    };

    if let Some(mut c) = capture.take() {
        // Capture first: the compositor then sees the end of input and closes the encoder's.
        let _ = c.child.start_kill();
        let mixer_result = match c.mixer.take() {
            Some(m) => Some(tokio::time::timeout(Duration::from_secs(3), m).await),
            None => None,
        };
        if std::env::var_os("XSPECTOR_DEBUG").is_some() {
            eprintln!("[mixer ch{}] ended: {mixer_result:?}; capture log: {:?}", ch.id, tail.lock().unwrap());
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), c.err).await;
    }
    // The encoder finalizes its last file once its input ends; make sure it is really gone.
    if tokio::time::timeout(Duration::from_secs(15), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), seg_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), err_task).await;

    // A crash leaves the last segment unlisted; fragmented MP4 means it is still playable.
    let done = finalized.lock().unwrap().clone();
    if !ctx.cfg.preview_only {
        salvage(ctx, idx, &prefix, proc_start, &done).await;
    }

    let mut retry_now = false;
    if !stopped {
        let all = tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
        if let Some(kind) = ffmpeg::unsupported(&all) {
            // The camera told us what it can do; correct the setting and try again immediately.
            let mut ov = ctx.overrides.lock().unwrap();
            let o = ov.entry(ch.id).or_default();
            match kind {
                ffmpeg::Unsupported::Framerate => {
                    let modes = ffmpeg::parse_avfoundation_modes(&all);
                    let size = ch.size.as_deref().and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                    if !o.rejected_fps.contains(&ch.fps) {
                        o.rejected_fps.push(ch.fps);
                    }
                    if let Some(f) = ffmpeg::best_fps(&modes, size, ch.fps, &o.rejected_fps).filter(|f| *f != ch.fps) {
                        reason = format!("The camera cannot do {} fps; using {f} fps", ch.fps);
                        o.fps = Some(f);
                        retry_now = true;
                    }
                }
                ffmpeg::Unsupported::Size if !o.drop_size => {
                    reason = "The camera cannot do that frame size; using its default size".into();
                    o.drop_size = true;
                    retry_now = true;
                }
                _ => {}
            }
        }
        if !retry_now {
            if let Some(last) = tail.lock().unwrap().back() {
                reason = format!("{reason}: {last}");
            }
        }
    }
    Outcome { stopped, reason, ran: t0.elapsed(), retry_now }
}

/// Feed `\r`- or `\n`-terminated records (FFmpeg stats use `\r`) to `f`.
async fn read_records<R, F, Fut>(mut r: R, mut f: F)
where
    R: tokio::io::AsyncRead + Unpin,
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut buf = [0u8; 4096];
    let mut carry: Vec<u8> = vec![];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&buf[..n]);
        while let Some(pos) = carry.iter().position(|&b| b == b'\n' || b == b'\r') {
            let line: Vec<u8> = carry.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            if !s.is_empty() {
                f(s).await;
            }
        }
        if carry.len() > 64 * 1024 {
            carry.clear();
        }
    }
}

/// Move a finished staging file to `layout::final_path`. Returns false if it could not be moved.
async fn finalize(
    ctx: &Ctx,
    idx: usize,
    name: &str,
    proc_start: DateTime<Local>,
    start_rel: f64,
) -> bool {
    let ch = &ctx.cfg.channels[idx];
    let src = ctx.staging.join(name);
    let offset = (proc_start - ctx.session_start).num_milliseconds() as f64 / 1000.0 + start_rel;
    let slot = layout::slot_for(offset, ctx.cfg.segment_secs);
    let slot_start = layout::slot_time(ctx.session_start, slot, ctx.cfg.segment_secs);
    let dest = layout::unique(layout::final_path(
        &ctx.cfg.root,
        ctx.cfg.split_layout,
        ctx.session_start,
        slot_start,
        ch.id,
    ));
    let moved = async {
        tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
        tokio::fs::rename(&src, &dest).await
    }
    .await;
    match moved {
        Ok(()) => {
            let shown = dest.to_string_lossy().into_owned();
            ctx.update(idx, |s| {
                s.segments += 1;
                s.last_file = Some(shown);
            });
            true
        }
        Err(e) => {
            ctx.update(idx, |s| s.last_error = Some(format!("move {name}: {e}")));
            false
        }
    }
}

async fn salvage(
    ctx: &Ctx,
    idx: usize,
    prefix: &str,
    proc_start: DateTime<Local>,
    done: &HashSet<String>,
) {
    let Ok(mut rd) = tokio::fs::read_dir(&ctx.staging).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(prefix) || done.contains(&name) {
            continue;
        }
        let len = e.metadata().await.map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            let _ = tokio::fs::remove_file(e.path()).await;
            continue;
        }
        // "CH1_p0_00003.mp4" -> segment index 3 -> approximate start
        let n: f64 = name
            .trim_start_matches(prefix)
            .trim_end_matches(".mp4")
            .parse()
            .unwrap_or(0.0);
        finalize(ctx, idx, &name, proc_start, n * ctx.cfg.segment_secs as f64).await;
    }
}

/// Files left in staging by an earlier crashed run are kept under `root/recovered/`.
async fn recover_leftovers(cfg: &RecordConfig, staging: &std::path::Path) {
    let Ok(mut rd) = tokio::fs::read_dir(staging).await else { return };
    let dest = cfg.root.join("recovered");
    while let Ok(Some(e)) = rd.next_entry().await {
        let len = e.metadata().await.map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            let _ = tokio::fs::remove_file(e.path()).await;
        } else {
            let _ = tokio::fs::create_dir_all(&dest).await;
            let target = layout::unique(dest.join(e.file_name()));
            let _ = tokio::fs::rename(e.path(), target).await;
        }
    }
}

/// FFmpeg's `info`-level banner lines (the monitor logs at `info` once); not errors.
fn is_banner(line: &str) -> bool {
    let t = line.trim_start();
    ["Input #", "Output #", "Stream #", "Stream mapping", "Press [q]", "Metadata", "Duration", "encoder", "major_brand", "minor_version", "compatible_brands", "creation_time"]
        .iter()
        .any(|p| t.starts_with(p))
        || line.starts_with("    ")
        || t.starts_with("Stream")
}

/// Pixel formats learned from devices this session, keyed by device + mode.
fn device_formats() -> &'static Mutex<std::collections::HashMap<String, String>> {
    static M: std::sync::OnceLock<Mutex<std::collections::HashMap<String, String>>> = std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

fn device_format(name: &str, size: &str, fps: u32) -> Option<String> {
    device_formats().lock().unwrap().get(&format!("{name}|{size}|{fps}")).cloned()
}

fn remember_device_format(name: &str, size: &str, fps: u32, fmt: &str) {
    device_formats().lock().unwrap().insert(format!("{name}|{size}|{fps}"), fmt.to_string());
}
