use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoder {
    VideoToolbox,
    Nvenc,
    Qsv,
    Amf,
    /// Windows Media Foundation: vendor-neutral, uses a hardware encoder when there is one and
    /// Microsoft's built-in software encoder otherwise. LGPL-friendly, so the Windows build relies on it.
    MediaFoundation,
    /// Software; GPL in FFmpeg, so a shipped LGPL build will not have it.
    X264,
}

impl Encoder {
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Encoder::VideoToolbox => "h264_videotoolbox",
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Qsv => "h264_qsv",
            Encoder::Amf => "h264_amf",
            Encoder::MediaFoundation => "h264_mf",
            Encoder::X264 => "libx264",
        }
    }

    /// Output-side codec arguments (after the filter chain).
    pub fn args(self, bitrate: &str) -> Vec<String> {
        let mut a = vec!["-c:v".to_string(), self.ffmpeg_name().to_string()];
        let extra: &[&str] = match self {
            Encoder::VideoToolbox => &["-realtime", "1", "-prio_speed", "1"],
            Encoder::Nvenc => &["-preset", "p4", "-tune", "ll"],
            Encoder::Qsv | Encoder::Amf | Encoder::MediaFoundation => &[],
            Encoder::X264 => &["-preset", "veryfast", "-sc_threshold", "0"],
        };
        a.extend(extra.iter().map(|s| s.to_string()));
        a.extend(["-b:v".to_string(), bitrate.to_string()]);
        a
    }

    /// Preference order per platform: hardware first.
    pub fn candidates() -> &'static [Encoder] {
        if cfg!(target_os = "macos") {
            &[Encoder::VideoToolbox, Encoder::X264]
        } else {
            &[Encoder::Nvenc, Encoder::Amf, Encoder::Qsv, Encoder::MediaFoundation, Encoder::X264]
        }
    }
}

/// Encode one second of synthetic video; only an encoder that really works on this machine passes
/// (a compiled-in NVENC without an NVIDIA GPU would fail here), same idea as OBS' `*-test.exe`.
pub async fn works(ffmpeg: &Path, enc: Encoder) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=1280x720:rate=30")
        .args(["-t", "1", "-pix_fmt", "yuv420p", "-c:v", enc.ffmpeg_name(), "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .is_ok_and(|s| s.success())
}

pub async fn usable(ffmpeg: &Path) -> Vec<Encoder> {
    let mut out = vec![];
    for &e in Encoder::candidates() {
        if works(ffmpeg, e).await {
            out.push(e);
        }
    }
    out
}

pub async fn best(ffmpeg: &Path) -> Result<Encoder> {
    for &e in Encoder::candidates() {
        if works(ffmpeg, e).await {
            return Ok(e);
        }
    }
    Err(Error::Ffmpeg("no working H.264 encoder".into()))
}

/// Does this FFmpeg build have the named filter? (`drawtext` needs a build with libfreetype.)
pub async fn has_filter(ffmpeg: &Path, name: &str) -> bool {
    let Ok(out) = Command::new(ffmpeg).args(["-hide_banner", "-filters"]).stdin(Stdio::null()).output().await else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some(name))
}

/// Does this FFmpeg build have the named decoder? (`png` needs zlib at build time.)
pub async fn has_decoder(ffmpeg: &Path, name: &str) -> bool {
    let Ok(out) = Command::new(ffmpeg).args(["-hide_banner", "-decoders"]).stdin(Stdio::null()).output().await else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some(name))
}
