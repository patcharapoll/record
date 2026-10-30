//! Live preview frames, raw and uncompressed, for GPU rendering in the UI.
//!
//! FFmpeg writes a YUV 4:2:0 `yuv4mpegpipe` stream to a loopback socket owned by the recorder
//! ([`relay`]). Each frame is republished on a [`PreviewHub`] as one self-describing message, and the
//! app forwards it to the UI, where a WebGL shader converts YUV to RGB and scales it on the GPU
//! (the same idea as OBS' non-"fast path" camera capture: raw YUV in, conversion in a shader).
//!
//! Message layout (little endian): `u32 width, u32 height, u32 format, u64 unix_ms`, then the pixels:
//! * format 0, planar YUV 4:2:0: Y plane (`w*h`), U plane, V plane (`((w+1)/2) * ((h+1)/2)` each)
//! * format 1, packed UYVY 4:2:2 (`w*h*2` bytes)
//! * format 2, packed YUYV 4:2:2 (`w*h*2` bytes)
//! * format 3, NV12: Y plane (`w*h`) then interleaved UV (`w*h/2`)
//!
//! Camera-native formats (1-3) skip any CPU conversion in FFmpeg; the shader unpacks them.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// One frame message (see the module docs for the layout).
pub type Frame = Arc<Vec<u8>>;

type Slot = (broadcast::Sender<Frame>, Option<Frame>);

pub const HEADER_LEN: usize = 20;

/// Default widest preview we send (wider cameras are scaled down). Moving frames to the UI is a
/// real cost: measured on a 720p camera, 640 wide used about a third less CPU than full size.
pub const DEFAULT_MAX_WIDTH: u32 = 640;

/// Pixel layout of the raw frames FFmpeg sends us (no header in the stream, so it must be known).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFmt {
    Uyvy,
    Yuyv,
    Nv12,
}

impl RawFmt {
    pub fn from_pix_fmt(name: &str) -> Option<Self> {
        match name {
            "uyvy422" => Some(Self::Uyvy),
            "yuyv422" => Some(Self::Yuyv),
            "nv12" => Some(Self::Nv12),
            _ => None,
        }
    }
    pub fn pix_fmt(self) -> &'static str {
        match self {
            Self::Uyvy => "uyvy422",
            Self::Yuyv => "yuyv422",
            Self::Nv12 => "nv12",
        }
    }
    pub fn code(self) -> u32 {
        match self {
            Self::Uyvy => 1,
            Self::Yuyv => 2,
            Self::Nv12 => 3,
        }
    }
    pub fn frame_bytes(self, w: u32, h: u32) -> usize {
        let (w, h) = (w as usize, h as usize);
        match self {
            Self::Uyvy | Self::Yuyv => w * h * 2,
            Self::Nv12 => w * h + w * h.div_ceil(2) ,
        }
    }
}

/// What the relay should expect on its socket.
#[derive(Debug, Clone, Copy)]
pub enum StreamKind {
    /// `yuv4mpegpipe`: self-describing, planar 4:2:0.
    Y4m,
    /// Headerless `rawvideo` of a known size and layout.
    Raw { w: u32, h: u32, fmt: RawFmt },
}

#[derive(Default)]
pub struct PreviewHub {
    chans: Mutex<HashMap<u32, Slot>>,
}

impl PreviewHub {
    /// Number of viewers currently attached to a channel.
    pub fn viewers(&self, ch: u32) -> usize {
        self.chans.lock().unwrap().get(&ch).map_or(0, |e| e.0.receiver_count())
    }

    pub fn publish_frame(&self, ch: u32, frame: Frame) {
        self.publish(ch, frame)
    }

    fn publish(&self, ch: u32, frame: Frame) {
        let mut g = self.chans.lock().unwrap();
        let e = g.entry(ch).or_insert_with(|| (broadcast::channel(2).0, None));
        e.1 = Some(frame.clone());
        let _ = e.0.send(frame); // nobody listening is fine
    }

    /// New receiver for a channel plus the most recent frame (so a viewer sees something at once).
    pub fn subscribe(&self, ch: u32) -> (broadcast::Receiver<Frame>, Option<Frame>) {
        let mut g = self.chans.lock().unwrap();
        let e = g.entry(ch).or_insert_with(|| (broadcast::channel(2).0, None));
        (e.0.subscribe(), e.1.clone())
    }
}

/// Accept FFmpeg's connection and republish every frame it sends. Ends when FFmpeg disconnects.
pub fn relay(listener: TcpListener, ch: u32, hub: Arc<PreviewHub>, kind: StreamKind) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(Ok((sock, _))) = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await else { return };
        let _ = match kind {
            StreamKind::Y4m => read_frames(sock, |f| hub.publish(ch, f)).await,
            StreamKind::Raw { w, h, fmt } => read_raw(sock, w, h, fmt, |f| hub.publish(ch, f)).await,
        };
    })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// `YUV4MPEG2 W1280 H720 F30:1 Ip A1:1 C420jpeg` -> (1280, 720)
fn parse_y4m_header(line: &str) -> Option<(u32, u32)> {
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

async fn read_frames(sock: TcpStream, mut on_frame: impl FnMut(Frame)) -> io::Result<()> {
    let mut r = BufReader::with_capacity(256 * 1024, sock);
    let mut line = String::new();
    if r.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let Some((w, h)) = parse_y4m_header(&line) else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a y4m stream"));
    };
    if w > 8192 || h > 8192 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
    let body = w as usize * h as usize + 2 * cw * ch;
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 || !line.starts_with("FRAME") {
            return Ok(());
        }
        let mut msg = vec![0u8; HEADER_LEN + body];
        write_header(&mut msg, w, h, 0);
        r.read_exact(&mut msg[HEADER_LEN..]).await?;
        stamp(&mut msg);
        on_frame(Arc::new(msg));
    }
}

/// Fill in the header of a message whose pixels are already in place after [`HEADER_LEN`] bytes.
pub fn fill_header(msg: &mut [u8], w: u32, h: u32, fmt: u32) {
    write_header(msg, w, h, fmt);
    stamp(msg);
}

/// A complete frame message from raw planes (`fmt` as in the module docs).
pub fn build_message(w: u32, h: u32, fmt: u32, pixels: &[u8]) -> Vec<u8> {
    let mut msg = vec![0u8; HEADER_LEN + pixels.len()];
    write_header(&mut msg, w, h, fmt);
    msg[HEADER_LEN..].copy_from_slice(pixels);
    stamp(&mut msg);
    msg
}

fn write_header(msg: &mut [u8], w: u32, h: u32, fmt: u32) {
    msg[0..4].copy_from_slice(&w.to_le_bytes());
    msg[4..8].copy_from_slice(&h.to_le_bytes());
    msg[8..12].copy_from_slice(&fmt.to_le_bytes());
}

fn stamp(msg: &mut [u8]) {
    msg[12..20].copy_from_slice(&now_ms().to_le_bytes());
}

async fn read_raw(sock: TcpStream, w: u32, h: u32, fmt: RawFmt, mut on_frame: impl FnMut(Frame)) -> io::Result<()> {
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame size"));
    }
    let mut r = BufReader::with_capacity(256 * 1024, sock);
    let body = fmt.frame_bytes(w, h);
    loop {
        let mut msg = vec![0u8; HEADER_LEN + body];
        write_header(&mut msg, w, h, fmt.code());
        if r.read_exact(&mut msg[HEADER_LEN..]).await.is_err() {
            return Ok(()); // FFmpeg closed the socket
        }
        stamp(&mut msg);
        on_frame(Arc::new(msg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn header_is_parsed() {
        assert_eq!(parse_y4m_header("YUV4MPEG2 W1280 H720 F30:1 Ip A1:1 C420jpeg XYSCSS=420JPEG"), Some((1280, 720)));
        assert_eq!(parse_y4m_header("YUV4MPEG2 H720"), None);
        assert_eq!(parse_y4m_header("nope W1 H1"), None);
    }

    #[tokio::test]
    async fn relays_frames_as_self_describing_messages() {
        let hub = Arc::new(PreviewHub::default());
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        relay(l, 1, hub.clone(), StreamKind::Y4m);
        let (mut rx, _) = hub.subscribe(1);

        let mut ff = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // 3x3 frame: Y = 9 bytes, U and V = 2x2 = 4 bytes each (odd sizes round up).
        ff.write_all(b"YUV4MPEG2 W3 H3 F30:1 Ip C420jpeg\n").await.unwrap();
        for n in 0..2u8 {
            ff.write_all(b"FRAME\n").await.unwrap();
            ff.write_all(&[n; 9 + 8]).await.unwrap();
        }
        for n in 0..2u8 {
            let f = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
            assert_eq!(u32::from_le_bytes(f[0..4].try_into().unwrap()), 3);
            assert_eq!(u32::from_le_bytes(f[4..8].try_into().unwrap()), 3);
            assert_eq!(u32::from_le_bytes(f[8..12].try_into().unwrap()), 0);
            assert!(u64::from_le_bytes(f[12..20].try_into().unwrap()) > 1_600_000_000_000);
            assert_eq!(f.len(), HEADER_LEN + 17);
            assert!(f[HEADER_LEN..].iter().all(|b| *b == n));
        }
    }

    #[test]
    fn raw_frame_sizes() {
        assert_eq!(RawFmt::Uyvy.frame_bytes(1280, 720), 1280 * 720 * 2);
        assert_eq!(RawFmt::Nv12.frame_bytes(1280, 720), 1280 * 720 * 3 / 2);
        assert_eq!(RawFmt::from_pix_fmt("uyvy422"), Some(RawFmt::Uyvy));
        assert_eq!(RawFmt::from_pix_fmt("rgb24"), None);
    }

    #[tokio::test]
    async fn raw_stream_is_cut_into_fixed_size_frames() {
        let hub = Arc::new(PreviewHub::default());
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        relay(l, 2, hub.clone(), StreamKind::Raw { w: 4, h: 2, fmt: RawFmt::Uyvy });
        let (mut rx, _) = hub.subscribe(2);
        let collector = tokio::spawn(async move {
            let mut got = vec![];
            while got.len() < 3 {
                if let Ok(f) = rx.recv().await {
                    got.push(f);
                }
            }
            got
        });
        let mut ff = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // Frames are 16 bytes; send them in awkward pieces.
        let stream: Vec<u8> = (0..3u8).flat_map(|n| [n; 16]).collect();
        for chunk in stream.chunks(5) {
            ff.write_all(chunk).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let got = tokio::time::timeout(Duration::from_secs(2), collector).await.unwrap().unwrap();
        for (n, f) in got.iter().enumerate() {
            let n = n as u8;
            assert_eq!(u32::from_le_bytes(f[8..12].try_into().unwrap()), 1);
            assert_eq!(f.len(), HEADER_LEN + 16);
            assert!(f[HEADER_LEN..].iter().all(|b| *b == n));
        }
    }

    #[tokio::test]
    async fn garbage_stream_is_rejected_quietly() {
        let hub = Arc::new(PreviewHub::default());
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = relay(l, 1, hub, StreamKind::Y4m);
        let mut ff = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        ff.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), h).await.unwrap().unwrap();
    }
}
