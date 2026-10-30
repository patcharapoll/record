use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::overlay::OverlayConfig;
use crate::preview::PreviewHub;
use crate::probe::Encoder;
use crate::storage::StorageConfig;
use crate::{Error, Result};

/// Where a rotated file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SplitLayout {
    /// `root/<slot-time>/CHn.mp4`, a new folder for every segment.
    #[default]
    NewFolder,
    /// `root/<session-start>/CHn_<slot-time>.mp4`, one folder per recording session.
    SameFolder,
}

/// How interlaced (analog) video is made progressive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Deinterlace {
    #[default]
    Off,
    /// `bwdif` (LGPL): measured at about 5% of a core per 720p30 channel.
    On,
}

// Older settings stored true/false or "fast"/"quality".
impl<'de> Deserialize<'de> for Deinterlace {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Text(String),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Bool(true) => Deinterlace::On,
            Raw::Bool(false) => Deinterlace::Off,
            Raw::Text(t) if matches!(t.as_str(), "on" | "fast" | "quality") => Deinterlace::On,
            Raw::Text(_) => Deinterlace::Off,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Source {
    /// A capture device by the name FFmpeg lists (avfoundation / dshow).
    Device(String),
    /// Synthetic test pattern, lets the whole pipeline run without hardware.
    TestPattern,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// 1-based, used for the `CHn` file name.
    pub id: u32,
    pub source: Source,
    /// `WIDTHxHEIGHT`; `None` lets the device pick its default.
    pub size: Option<String>,
    pub fps: u32,
    #[serde(default)]
    pub deinterlace: Deinterlace,
    /// Frame size the live monitor saw on this device. Lets the overlay layer match the video
    /// exactly, which avoids a scaling step on every frame.
    #[serde(default)]
    pub detected_size: Option<(u32, u32)>,
    /// Pixel format the device delivers (e.g. `uyvy422`), as seen by the live monitor.
    #[serde(default)]
    pub detected_format: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RecordConfig {
    pub ffmpeg_path: PathBuf,
    pub root: PathBuf,
    /// Segment length in seconds. Default 30 minutes.
    pub segment_secs: u64,
    pub split_layout: SplitLayout,
    /// `None` = auto-detect the best working encoder.
    pub encoder: Option<Encoder>,
    pub bitrate: String,
    pub storage: StorageConfig,
    /// When set, each channel also writes a small JPEG (`CHn.jpg`, ~2 fps) here for the UI.
    pub preview_dir: Option<PathBuf>,
    pub overlay: OverlayConfig,
    /// Set by the recorder: where the overlay text files live while recording.
    #[serde(skip)]
    pub overlay_dir: Option<PathBuf>,
    /// Live monitor: only the small preview JPEG is produced, nothing is encoded or recorded.
    /// Lets the camera show up before recording starts. Needs `preview_dir`.
    #[serde(skip)]
    pub preview_only: bool,
    /// Where preview frames are published as an MJPEG stream (smooth). Without it the preview is
    /// a single JPEG file rewritten a few times per second.
    #[serde(skip)]
    pub preview_hub: Option<Arc<PreviewHub>>,
    /// Widest preview frame in pixels (the camera's own size is used when it is narrower).
    pub preview_max_width: u32,
    pub channels: Vec<ChannelConfig>,
}

impl std::fmt::Debug for RecordConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordConfig").field("root", &self.root).field("channels", &self.channels).finish_non_exhaustive()
    }
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            ffmpeg_path: PathBuf::from("ffmpeg"),
            root: PathBuf::from("recordings"),
            segment_secs: 30 * 60,
            split_layout: SplitLayout::default(),
            encoder: None,
            bitrate: "4M".into(),
            storage: StorageConfig::default(),
            preview_dir: None,
            overlay: OverlayConfig::default(),
            overlay_dir: None,
            preview_only: false,
            preview_hub: None,
            preview_max_width: crate::preview::DEFAULT_MAX_WIDTH,
            channels: vec![],
        }
    }
}

impl RecordConfig {
    pub fn validate(&self) -> Result<()> {
        if self.preview_only && self.preview_dir.is_none() {
            return Err(Error::Config("preview_only needs preview_dir".into()));
        }
        if self.channels.is_empty() {
            return Err(Error::Config("no channels".into()));
        }
        if self.segment_secs < 10 {
            return Err(Error::Config("segment_secs must be >= 10".into()));
        }
        let mut ids: Vec<_> = self.channels.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() != self.channels.len() || ids.contains(&0) {
            return Err(Error::Config("channel ids must be unique and >= 1".into()));
        }
        for c in &self.channels {
            if c.fps == 0 || c.fps > 120 {
                return Err(Error::Config(format!("CH{} fps out of range", c.id)));
            }
            if let Some(s) = &c.size {
                let ok = s.split_once('x').is_some_and(|(w, h)| {
                    w.parse::<u32>().is_ok_and(|v| v > 0) && h.parse::<u32>().is_ok_and(|v| v > 0)
                });
                if !ok {
                    return Err(Error::Config(format!("CH{} size must be WxH", c.id)));
                }
            }
        }
        Ok(())
    }
}
