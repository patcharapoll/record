//! The compositor between capture and encoding, used for the live (editable) overlay.
//!
//! ```text
//! capture ffmpeg --y4m--> [ this module: overlay blended in, preview published, paced to a fixed
//!                           frame rate ] --y4m--> encoder ffmpeg (segments, MP4)
//! ```
//!
//! Doing the compositing here, not in an FFmpeg filter graph, avoids FFmpeg's `overlay` waiting for
//! its second input: with a live camera that made the camera's frames queue up and the device then
//! dropped most of them (measured: ~10 fps and mostly repeated frames). Here the overlay is just a
//! small picture blended onto each frame, the frame rate is exact, and the same frames feed the UI
//! preview, so the camera is opened only once.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::layer::Renderer;
use crate::overlay::{self, DataBus, ItemKind, OverlayConfig, OverlayItem};
use crate::preview::{self, PreviewHub};

/// Counters the recorder shows in the UI.
#[derive(Default)]
pub struct MixerStats {
    pub width: AtomicU64,
    pub height: AtomicU64,
    /// Frames from the camera that were sent on.
    pub fresh: AtomicU64,
    /// Ticks with no new camera frame (the previous one was sent again).
    pub dup: AtomicU64,
    /// Camera frames replaced before they were sent (camera faster than the output rate).
    pub dropped: AtomicU64,
    /// Milliseconds since the mixer started when the last camera frame arrived.
    pub last_frame_ms: AtomicU64,
}

pub struct MixerParams {
    pub channel: u32,
    pub fps: u32,
    pub hub: Option<Arc<PreviewHub>>,
    pub preview_max_width: u32,
    pub font_file: Option<PathBuf>,
    pub overlay_rx: watch::Receiver<OverlayConfig>,
    pub bus: DataBus,
}

// ---------------------------------------------------------------------------------------------
// Overlay as YUV planes
// ---------------------------------------------------------------------------------------------

/// The overlay converted to the video's pixel format, cropped to the area that has anything in it.
#[derive(Default)]
pub struct OverlayPlanes {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    y: Vec<u8>,
    a: Vec<u8>,
    cw: usize,
    ch: usize,
    u: Vec<u8>,
    v: Vec<u8>,
    ca: Vec<u8>,
}

impl OverlayPlanes {
    pub fn is_empty(&self) -> bool {
        self.w == 0
    }
}

/// Straight-alpha RGBA (`fw` x `fh`) -> planes for the box that contains every non-transparent pixel.
pub fn overlay_planes(rgba: &[u8], fw: usize, fh: usize) -> OverlayPlanes {
    let (mut x0, mut y0, mut x1, mut y1) = (fw, fh, 0usize, 0usize);
    for y in 0..fh {
        let row = &rgba[y * fw * 4..(y + 1) * fw * 4];
        if let (Some(f), Some(l)) = (
            row.chunks_exact(4).position(|p| p[3] != 0),
            row.chunks_exact(4).rposition(|p| p[3] != 0),
        ) {
            x0 = x0.min(f);
            x1 = x1.max(l + 1);
            y0 = y0.min(y);
            y1 = y1.max(y + 1);
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return OverlayPlanes::default();
    }
    // Even-aligned so chroma (half resolution) lines up.
    let (x0, y0) = (x0 & !1, y0 & !1);
    let (x1, y1) = (((x1 + 1) & !1).min(fw & !1), ((y1 + 1) & !1).min(fh & !1));
    let (w, h) = (x1 - x0, y1 - y0);
    let hd = fh >= 720;
    let px = |x: usize, y: usize| {
        let i = ((y0 + y) * fw + x0 + x) * 4;
        (rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32, rgba[i + 3] as i32)
    };
    let mut o = OverlayPlanes { x0, y0, w, h, cw: w / 2, ch: h / 2, ..Default::default() };
    o.y = vec![16; w * h];
    o.a = vec![0; w * h];
    for y in 0..h {
        for x in 0..w {
            let (r, g, b, a) = px(x, y);
            o.y[y * w + x] = (if hd { (47 * r + 157 * g + 16 * b + 128) >> 8 } else { (66 * r + 129 * g + 25 * b + 128) >> 8 } + 16) as u8;
            o.a[y * w + x] = a as u8;
        }
    }
    o.u = vec![128; o.cw * o.ch];
    o.v = vec![128; o.cw * o.ch];
    o.ca = vec![0; o.cw * o.ch];
    for cy in 0..o.ch {
        for cx in 0..o.cw {
            // average the 2x2 block, weighted by alpha so transparent pixels do not tint the colour
            let (mut sr, mut sg, mut sb, mut sa) = (0, 0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (r, g, b, a) = px(cx * 2 + dx, cy * 2 + dy);
                sr += r * a;
                sg += g * a;
                sb += b * a;
                sa += a;
            }
            let i = cy * o.cw + cx;
            o.ca[i] = (sa / 4) as u8;
            if sa > 0 {
                let (r, g, b) = (sr / sa, sg / sa, sb / sa);
                let (u, v) = if hd {
                    (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128, ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128)
                } else {
                    (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128, ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128)
                };
                o.u[i] = u.clamp(0, 255) as u8;
                o.v[i] = v.clamp(0, 255) as u8;
            }
        }
    }
    o
}

#[inline]
fn mix(base: u8, over: u8, a: u8) -> u8 {
    match a {
        0 => base,
        255 => over,
        _ => (base as i32 + (((over as i32 - base as i32) * a as i32 + 127) / 255)) as u8,
    }
}

/// Blend into a planar 4:2:0 frame body (`Y`, `U`, `V` planes) of `fw` x `fh`, in place.
pub fn blend(frame: &mut [u8], fw: usize, fh: usize, o: &OverlayPlanes) {
    if o.is_empty() || o.x0 + o.w > fw || o.y0 + o.h > fh {
        return;
    }
    let (cfw, cfh) = (fw.div_ceil(2), fh.div_ceil(2));
    let (yp, rest) = frame.split_at_mut(fw * fh);
    let (up, vp) = rest.split_at_mut(cfw * cfh);
    for y in 0..o.h {
        let dst = &mut yp[(o.y0 + y) * fw + o.x0..(o.y0 + y) * fw + o.x0 + o.w];
        let (src, al) = (&o.y[y * o.w..(y + 1) * o.w], &o.a[y * o.w..(y + 1) * o.w]);
        for ((d, s), a) in dst.iter_mut().zip(src).zip(al) {
            *d = mix(*d, *s, *a);
        }
    }
    let (cx0, cy0) = (o.x0 / 2, o.y0 / 2);
    for y in 0..o.ch {
        let r = (cy0 + y) * cfw + cx0;
        for x in 0..o.cw {
            let a = o.ca[y * o.cw + x];
            up[r + x] = mix(up[r + x], o.u[y * o.cw + x], a);
            vp[r + x] = mix(vp[r + x], o.v[y * o.cw + x], a);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Compositor task: re-renders the overlay when it changes (at most 5 times a second)
// ---------------------------------------------------------------------------------------------

fn spawn_compositor(
    w: u32,
    h: u32,
    p: &MixerParams,
) -> (watch::Receiver<Arc<OverlayPlanes>>, JoinHandle<()>) {
    let (tx, rx) = watch::channel(Arc::new(OverlayPlanes::default()));
    let (mut cfg_rx, bus, font, channel) = (p.overlay_rx.clone(), p.bus.clone(), p.font_file.clone(), p.channel);
    let task = tokio::spawn(async move {
        // Loading fonts takes a moment; keep it off the async threads.
        let Ok(mut renderer) = tokio::task::spawn_blocking(move || Renderer::new(w, h, font.as_deref())).await else { return };
        let mut last = String::from("\u{0}");
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tick.tick().await;
            let cfg = cfg_rx.borrow_and_update().clone();
            let data = bus.snapshot();
            let items: Vec<OverlayItem> = if cfg.enabled { cfg.items.clone() } else { vec![] };
            let mut sig = serde_json::to_string(&items).unwrap_or_default();
            for it in items.iter().filter(|i| i.enabled && i.kind == ItemKind::Text) {
                sig.push('\u{1}');
                sig.push_str(&overlay::render(&it.text, channel, &data));
            }
            if sig == last {
                continue;
            }
            last = sig;
            let rgba = renderer.render(&items, channel, &data);
            let planes = overlay_planes(&rgba, w as usize, h as usize);
            if tx.send(Arc::new(planes)).is_err() {
                return;
            }
        }
    });
    (rx, task)
}

// ---------------------------------------------------------------------------------------------
// Preview message
// ---------------------------------------------------------------------------------------------

/// Reduce a plane (`sw` wide) by an integer factor `f` with a box average, to exactly `ow` x `oh`.
fn shrink(src: &[u8], sw: usize, f: usize, ow: usize, oh: usize) -> Vec<u8> {
    let mut out = vec![0u8; ow * oh];
    if f == 2 {
        // the common case (720p -> 640 wide), written so the compiler can vectorize it
        for y in 0..oh {
            let (r0, r1) = (&src[2 * y * sw..2 * y * sw + 2 * ow], &src[(2 * y + 1) * sw..(2 * y + 1) * sw + 2 * ow]);
            for ((o, a), b) in out[y * ow..(y + 1) * ow].iter_mut().zip(r0.chunks_exact(2)).zip(r1.chunks_exact(2)) {
                *o = ((a[0] as u16 + a[1] as u16 + b[0] as u16 + b[1] as u16 + 2) >> 2) as u8;
            }
        }
        return out;
    }
    let area = (f * f) as u32;
    for y in 0..oh {
        for x in 0..ow {
            let mut s = 0u32;
            for dy in 0..f {
                let row = &src[(y * f + dy) * sw + x * f..(y * f + dy) * sw + x * f + f];
                s += row.iter().map(|v| *v as u32).sum::<u32>();
            }
            out[y * ow + x] = ((s + area / 2) / area) as u8;
        }
    }
    out
}

/// Frame message for the UI (see `preview.rs`), reduced to at most `max_w` wide.
pub fn preview_message(body: &[u8], w: usize, h: usize, max_w: usize) -> Vec<u8> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let f = w.div_ceil(max_w).max(1);
    let (y, u, v) = (&body[..w * h], &body[w * h..w * h + cw * ch], &body[w * h + cw * ch..]);
    if f == 1 {
        let mut msg = vec![0u8; preview::HEADER_LEN];
        msg.reserve(body.len());
        msg.extend_from_slice(body);
        preview::fill_header(&mut msg, w as u32, h as u32, 0);
        return msg;
    }
    // keep dimensions even so chroma stays aligned
    let (ow, oh) = ((w / f) & !1, (h / f) & !1);
    let (cow, coh) = (ow / 2, oh / 2);
    let mut msg = vec![0u8; preview::HEADER_LEN];
    msg.reserve(ow * oh + 2 * cow * coh);
    msg.extend_from_slice(&shrink(y, w, f, ow, oh));
    msg.extend_from_slice(&shrink(u, cw, f, cow, coh));
    msg.extend_from_slice(&shrink(v, cw, f, cow, coh));
    preview::fill_header(&mut msg, ow as u32, oh as u32, 0);
    msg
}

// ---------------------------------------------------------------------------------------------
// The mixer
// ---------------------------------------------------------------------------------------------

/// Reuses frame buffers. Allocating 1.4 MB per frame costs page faults on every frame (measured:
/// more than half of this module's CPU); recycled buffers stay mapped.
struct Pool {
    free: std::sync::Mutex<Vec<Vec<u8>>>,
    len: usize,
}

impl Pool {
    fn take(&self) -> Vec<u8> {
        let mut v = self.free.lock().unwrap().pop().unwrap_or_default();
        v.clear();
        v.reserve(self.len);
        v
    }
}

/// A frame that returns its buffer to the pool when the last reference is dropped.
struct Pooled {
    data: Vec<u8>,
    pool: Arc<Pool>,
}

impl Drop for Pooled {
    fn drop(&mut self) {
        let mut free = self.pool.free.lock().unwrap();
        if free.len() < 8 {
            free.push(std::mem::take(&mut self.data));
        }
    }
}

fn parse_header(line: &str) -> Option<(u32, u32)> {
    let mut it = line.split_whitespace();
    if it.next()? != "YUV4MPEG2" {
        return None;
    }
    let (mut w, mut h) = (None, None);
    for t in it {
        match t.split_at(1) {
            ("W", v) => w = v.parse().ok(),
            ("H", v) => h = v.parse().ok(),
            _ => {}
        }
    }
    Some((w.filter(|v| *v > 0)?, h.filter(|v| *v > 0)?))
}

/// Move frames from the capture process's output to the encoder's input until the capture ends.
/// (Anonymous pipes are cheaper than loopback sockets for ~40 MB/s each way.)
pub async fn run<R, W>(capture: R, mut enc: W, p: MixerParams, stats: Arc<MixerStats>) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut r = BufReader::with_capacity(512 * 1024, capture);
    let mut line = String::new();
    let wait = Duration::from_secs(30);
    tokio::time::timeout(wait, r.read_line(&mut line))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "capture produced nothing"))??;
    let (w, h) = parse_header(&line).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a y4m stream"))?;
    if w > 8192 || h > 8192 || w % 2 != 0 || h % 2 != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported frame size"));
    }
    let (wu, hu) = (w as usize, h as usize);
    let body_len = wu * hu + 2 * (wu / 2) * (hu / 2);
    stats.width.store(w as u64, Ordering::Relaxed);
    stats.height.store(h as u64, Ordering::Relaxed);

    enc.write_all(format!("YUV4MPEG2 W{w} H{h} F{}:1 Ip A1:1 C420jpeg\n", p.fps).as_bytes()).await?;

    let (ov_rx, comp) = spawn_compositor(w, h, &p);
    let started = tokio::time::Instant::now();

    // Writer: sends frames to the encoder in order. Decoupled by a small queue so a slow encoder
    // never stalls the camera read; if the queue is full the newest frame is dropped instead.
    let pool = Arc::new(Pool { free: Default::default(), len: body_len });
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Arc<Pooled>>(6);
    let writer = tokio::spawn(async move {
        while let Some(f) = rx.recv().await {
            if enc.write_all(b"FRAME\n").await.is_err() || enc.write_all(&f.data).await.is_err() {
                return; // encoder is gone
            }
        }
    });

    // Reader: camera frames in, overlay blended, preview published, output frame rate kept exact.
    //
    // Pacing follows the *camera's arrival times* rather than a free-running timer (a timer beats
    // against the camera clock and produces a repeated and a dropped frame several times a second).
    // Frame `e` is the output slot a frame belongs to; a gap leaves repeats of the previous frame,
    // a frame that arrives for a slot already used is dropped. Small jitter changes nothing.
    let period = 1.0 / p.fps.max(1) as f64;
    let mut t_first: Option<tokio::time::Instant> = None;
    let mut next_slot: u64 = 0;
    let mut previous: Option<Arc<Pooled>> = None;
    let mut line = String::new();
    let result = async {
        loop {
            line.clear();
            if r.read_line(&mut line).await? == 0 || !line.starts_with("FRAME") {
                return Ok::<(), io::Error>(());
            }
            let mut body: Vec<u8> = pool.take();
            while body.len() < body_len {
                if r.read_buf(&mut body).await? == 0 {
                    return Ok(());
                }
            }
            let now = tokio::time::Instant::now();
            stats.last_frame_ms.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);

            if let Some(hub) = p.hub.as_ref().filter(|h| h.viewers(p.channel) > 0) {
                // the UI draws its own overlay, so the preview is taken before blending
                hub.publish_frame(p.channel, Arc::new(preview_message(&body, wu, hu, p.preview_max_width as usize)));
            }

            let t0 = *t_first.get_or_insert(now);
            let slot = ((now - t0).as_secs_f64() / period + 0.5).floor() as u64;
            if slot < next_slot {
                stats.dropped.fetch_add(1, Ordering::Relaxed); // camera ahead of the output rate
                continue;
            }
            let planes = ov_rx.borrow().clone();
            blend(&mut body, wu, hu, &planes);
            let frame = Arc::new(Pooled { data: body, pool: pool.clone() });
            if let Some(prev) = &previous {
                // repeat the last frame for every slot nobody filled
                for _ in next_slot..slot {
                    stats.dup.fetch_add(1, Ordering::Relaxed);
                    if tx.try_send(prev.clone()).is_err() {
                        break;
                    }
                }
            }
            match tx.try_send(frame.clone()) {
                Ok(()) => {
                    stats.fresh.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    stats.dropped.fetch_add(1, Ordering::Relaxed); // encoder cannot keep up
                }
            }
            previous = Some(frame);
            next_slot = slot + 1;
        }
    }
    .await;

    drop(tx); // lets the writer finish what is queued, then drops the encoder socket
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    comp.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, y: u8) -> Vec<u8> {
        let mut f = vec![y; w * h];
        f.extend(vec![128; 2 * (w / 2) * (h / 2)]);
        f
    }

    #[test]
    fn opaque_box_replaces_pixels_only_inside_it() {
        // 8x8 canvas with an opaque white 4x2 box at (2,2)
        let (fw, fh) = (8, 8);
        let mut rgba = vec![0u8; fw * fh * 4];
        for y in 2..4 {
            for x in 2..6 {
                rgba[(y * fw + x) * 4..(y * fw + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let o = overlay_planes(&rgba, fw, fh);
        assert!(!o.is_empty());
        let mut f = frame(fw, fh, 50);
        blend(&mut f, fw, fh, &o);
        assert_eq!(f[2 * fw + 2], 235, "white is Y=235");
        assert_eq!(f[0], 50, "outside the box is untouched");
        assert_eq!(f[3 * fw + 5], 235);
        assert_eq!(f[4 * fw + 2], 50);
        // chroma of the covered block stays neutral for white
        let u0 = fw * fh;
        assert!((f[u0 + 1 * 4 + 1] as i32 - 128).abs() <= 1);
    }

    #[test]
    fn half_transparent_black_darkens_halfway() {
        let (fw, fh) = (4, 4);
        let rgba: Vec<u8> = (0..fw * fh).flat_map(|_| [0u8, 0, 0, 128]).collect();
        let o = overlay_planes(&rgba, fw, fh);
        let mut f = frame(fw, fh, 200);
        blend(&mut f, fw, fh, &o);
        // halfway between 200 and black (Y=16): about 108
        assert!((f[0] as i32 - 108).abs() <= 2, "{}", f[0]);
    }

    #[test]
    fn empty_overlay_changes_nothing() {
        let o = overlay_planes(&vec![0u8; 4 * 4 * 4], 4, 4);
        assert!(o.is_empty());
        let mut f = frame(4, 4, 99);
        let before = f.clone();
        blend(&mut f, 4, 4, &o);
        assert_eq!(f, before);
    }

    #[test]
    fn preview_is_shrunk_to_the_requested_width() {
        let (w, h) = (16, 8);
        let body = frame(w, h, 100);
        let m = preview_message(&body, w, h, 8);
        assert_eq!(u32::from_le_bytes(m[0..4].try_into().unwrap()), 8);
        assert_eq!(u32::from_le_bytes(m[4..8].try_into().unwrap()), 4);
        assert_eq!(m.len(), preview::HEADER_LEN + 8 * 4 + 2 * 4 * 2);
        assert!(m[preview::HEADER_LEN..preview::HEADER_LEN + 32].iter().all(|v| *v == 100));
        // no shrinking when it already fits
        let same = preview_message(&body, w, h, 64);
        assert_eq!(u32::from_le_bytes(same[0..4].try_into().unwrap()), 16);
    }

    #[tokio::test]
    async fn moves_frames_from_capture_to_encoder_at_a_fixed_rate() {
        // in-memory pipes stand in for the two ffmpeg processes
        let (mut cap_tx, cap_rx) = tokio::io::duplex(1 << 20);
        let (enc_tx, mut enc_rx) = tokio::io::duplex(1 << 20);
        let (otx, orx) = watch::channel(OverlayConfig::default());
        drop(otx);
        let stats = Arc::new(MixerStats::default());
        let params = MixerParams { channel: 1, fps: 25, hub: None, preview_max_width: 640, font_file: None, overlay_rx: orx, bus: DataBus::default() };
        let mixer = tokio::spawn(run(cap_rx, enc_tx, params, stats.clone()));

        let reader = tokio::spawn(async move {
            let mut r = BufReader::new(&mut enc_rx);
            let mut header = String::new();
            r.read_line(&mut header).await.unwrap();
            let mut frames = vec![];
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).await.unwrap() == 0 {
                    break;
                }
                let mut b = vec![0u8; 16 * 16 * 3 / 2];
                r.read_exact(&mut b).await.unwrap();
                frames.push(b[0]);
            }
            (header, frames)
        });
        // fake capture: header + 5 frames, one every 80 ms (slower than 25 fps -> repeats)
        cap_tx.write_all(b"YUV4MPEG2 W16 H16 F12:1 Ip C420jpeg\n").await.unwrap();
        for n in 0..5u8 {
            cap_tx.write_all(b"FRAME\n").await.unwrap();
            cap_tx.write_all(&frame(16, 16, 100 + n)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        drop(cap_tx);
        let (header, frames) = tokio::time::timeout(Duration::from_secs(3), reader).await.unwrap().unwrap();
        mixer.await.unwrap().unwrap();
        assert!(header.starts_with("YUV4MPEG2 W16 H16 F25:1"), "{header}");
        // 5 camera frames over ~0.4 s at 25 fps -> about 9 output frames, repeats included, in order
        assert!(frames.len() >= 8, "{frames:?}");
        assert!(frames.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*frames.first().unwrap(), 100);
        assert!(stats.dup.load(Ordering::Relaxed) >= 3);
        assert_eq!(stats.fresh.load(Ordering::Relaxed), 5);
    }
}
