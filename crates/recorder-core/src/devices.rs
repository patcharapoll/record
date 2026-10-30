use std::path::Path;
use std::process::Stdio;

use serde::Serialize;
use tokio::process::Command;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VideoDevice {
    pub name: String,
}

/// FFmpeg lists devices on stderr and exits non-zero; that is normal.
pub async fn list_video(ffmpeg: &Path) -> Result<Vec<VideoDevice>> {
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["-f", "avfoundation", "-list_devices", "true", "-i", ""]
    } else {
        &["-list_devices", "true", "-f", "dshow", "-i", "dummy"]
    };
    let out = Command::new(ffmpeg)
        .arg("-hide_banner")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::Ffmpeg(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stderr);
    Ok(if cfg!(target_os = "macos") { parse_avfoundation(&text) } else { parse_dshow(&text) })
}

/// Modes (sizes and frame rates) a capture device offers. FFmpeg only reveals them in error output,
/// so ask for a mode that cannot exist and read the list it prints.
pub async fn list_modes(ffmpeg: &Path, name: &str) -> Result<Vec<crate::ffmpeg::Mode>> {
    let args: Vec<String> = if cfg!(target_os = "macos") {
        ["-f", "avfoundation", "-framerate", "1", "-i"].map(String::from).into_iter().chain([format!("{name}:none")]).collect()
    } else {
        ["-list_options", "true", "-f", "dshow", "-i"].map(String::from).into_iter().chain([format!("video={name}")]).collect()
    };
    let out = Command::new(ffmpeg)
        .arg("-hide_banner")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::Ffmpeg(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stderr);
    let mut modes = if cfg!(target_os = "macos") {
        crate::ffmpeg::parse_avfoundation_modes(&text)
    } else {
        crate::ffmpeg::parse_dshow_modes(&text)
    };
    modes.sort_by_key(|m| std::cmp::Reverse(m.w as u64 * m.h as u64));
    Ok(modes)
}

/// Pixel format a device delivers at a given mode (e.g. `uyvy422`), read from FFmpeg's banner after
/// opening it for a single frame. Cached for the session by the caller. `None` if it cannot be read.
pub async fn probe_pix_fmt(ffmpeg: &Path, name: &str, size: Option<&str>, fps: u32) -> Option<String> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "info".into()];
    if cfg!(target_os = "macos") {
        args.extend(["-f".into(), "avfoundation".into(), "-framerate".into(), fps.to_string()]);
        if let Some(s) = size {
            args.extend(["-video_size".into(), s.into()]);
        }
        args.extend(["-i".into(), format!("{name}:none")]);
    } else {
        args.extend(["-f".into(), "dshow".into(), "-framerate".into(), fps.to_string()]);
        if let Some(s) = size {
            args.extend(["-video_size".into(), s.into()]);
        }
        args.extend(["-i".into(), format!("video={name}")]);
    }
    args.extend(["-frames:v".into(), "1".into(), "-f".into(), "null".into(), "-".into()]);
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        Command::new(ffmpeg).args(args).stdin(Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    String::from_utf8_lossy(&out.stderr).lines().find_map(crate::ffmpeg::parse_video_pix_fmt)
}

fn strip_prefix(line: &str) -> &str {
    // "[AVFoundation indev @ 0x...] rest"
    line.split_once("] ").map_or(line, |(_, r)| r)
}

pub fn parse_avfoundation(text: &str) -> Vec<VideoDevice> {
    let mut in_video = false;
    let mut out = vec![];
    for line in text.lines() {
        let l = strip_prefix(line).trim();
        if l.contains("video devices") {
            in_video = true;
        } else if l.contains("audio devices") {
            in_video = false;
        } else if in_video {
            // "[0] MacBook Pro Camera"
            if let Some(rest) = l.strip_prefix('[') {
                if let Some((idx, name)) = rest.split_once("] ") {
                    if idx.chars().all(|c| c.is_ascii_digit()) && !name.starts_with("Capture screen") {
                        out.push(VideoDevice { name: name.trim().to_string() });
                    }
                }
            }
        }
    }
    out
}

pub fn parse_dshow(text: &str) -> Vec<VideoDevice> {
    let mut out = vec![];
    for line in text.lines() {
        let l = strip_prefix(line).trim();
        // `"USB Capture" (video)`; skip `Alternative name` lines
        if l.ends_with("(video)") || l.ends_with("(video, audio)") {
            if let (Some(a), Some(b)) = (l.find('"'), l.rfind('"')) {
                if b > a {
                    out.push(VideoDevice { name: l[a + 1..b].to_string() });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac() {
        let t = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
                 [AVFoundation indev @ 0x1] [0] MacBook Pro Camera\n\
                 [AVFoundation indev @ 0x1] [1] USB3.0 Capture\n\
                 [AVFoundation indev @ 0x1] [2] Capture screen 0\n\
                 [AVFoundation indev @ 0x1] AVFoundation audio devices:\n\
                 [AVFoundation indev @ 0x1] [0] MacBook Pro Microphone\n";
        let names: Vec<_> = parse_avfoundation(t).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["MacBook Pro Camera", "USB3.0 Capture"]);
    }

    #[test]
    fn windows() {
        let t = "[dshow @ 0x1] DirectShow video devices (some may be both video and audio devices)\n\
                 [dshow @ 0x1]  \"USB Video\" (video)\n\
                 [dshow @ 0x1]   Alternative name \"@device_pnp_\\\\?\\usb#vid_1\"\n\
                 [dshow @ 0x1] DirectShow audio devices\n\
                 [dshow @ 0x1]  \"Microphone\" (audio)\n";
        let names: Vec<_> = parse_dshow(t).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["USB Video"]);
    }
}
