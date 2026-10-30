//! Recording core: one FFmpeg child process per channel, rotated into files by a fixed
//! duration. The UI layer (Tauri) only talks to [`Recorder`]; all heavy work stays in FFmpeg.

pub mod config;
pub mod devices;
pub mod error;
pub mod ffmpeg;
pub mod layer;
pub mod layout;
pub mod mixer;
pub mod overlay;
pub mod preview;
pub mod probe;
pub mod recorder;
pub mod storage;

pub use config::{ChannelConfig, Deinterlace, RecordConfig, Source, SplitLayout};
pub use error::{Error, Result};
pub use recorder::{ChannelState, ChannelStatus, Recorder};
