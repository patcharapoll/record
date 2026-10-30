use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local};

use crate::SplitLayout;

/// `2025-04-09_22-13-08`
pub const TIME_FORMAT: &str = "%Y-%m-%d_%H-%M-%S";

/// Wall-clock start of segment slot `k`. Slots are aligned to the session start so every
/// channel names the same period identically, regardless of per-channel keyframe drift.
pub fn slot_time(session_start: DateTime<Local>, slot: u64, segment_secs: u64) -> DateTime<Local> {
    session_start + Duration::seconds((slot * segment_secs) as i64)
}

/// Nearest slot for a segment that started `offset` after the session start.
pub fn slot_for(offset_secs: f64, segment_secs: u64) -> u64 {
    (offset_secs / segment_secs as f64).round().max(0.0) as u64
}

pub fn final_path(
    root: &Path,
    layout: SplitLayout,
    session_start: DateTime<Local>,
    slot_start: DateTime<Local>,
    channel: u32,
) -> PathBuf {
    match layout {
        SplitLayout::NewFolder => root
            .join(slot_start.format(TIME_FORMAT).to_string())
            .join(format!("CH{channel}.mp4")),
        SplitLayout::SameFolder => root
            .join(session_start.format(TIME_FORMAT).to_string())
            .join(format!("CH{channel}_{}.mp4", slot_start.format(TIME_FORMAT))),
    }
}

/// Never overwrite: `CH1.mp4` -> `CH1_r1.mp4` -> `CH1_r2.mp4` ...
pub fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_owned();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("mp4").to_owned();
    (1..)
        .map(|n| path.with_file_name(format!("{stem}_r{n}.{ext}")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Local> {
        Local.with_ymd_and_hms(2025, 4, 9, 22, 13, 8).unwrap()
    }

    #[test]
    fn new_folder_matches_spec() {
        let p = final_path(Path::new("/r"), SplitLayout::NewFolder, t0(), t0(), 2);
        assert_eq!(p, Path::new("/r/2025-04-09_22-13-08/CH2.mp4"));
    }

    #[test]
    fn same_folder_uses_session_dir_and_slot_name() {
        let slot1 = slot_time(t0(), 1, 1800);
        let p = final_path(Path::new("/r"), SplitLayout::SameFolder, t0(), slot1, 1);
        assert_eq!(p, Path::new("/r/2025-04-09_22-13-08/CH1_2025-04-09_22-43-08.mp4"));
    }

    #[test]
    fn slot_rounds_keyframe_drift() {
        assert_eq!(slot_for(1801.9, 1800), 1);
        assert_eq!(slot_for(1798.0, 1800), 1);
        assert_eq!(slot_for(0.0, 1800), 0);
    }

    #[test]
    fn unique_adds_suffix() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("CH1.mp4");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(unique(p.clone()), d.path().join("CH1_r1.mp4"));
    }
}
