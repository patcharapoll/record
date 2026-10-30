//! Keeps long recordings from filling the disk: free-space floor with a choice of reaction, and an
//! optional maximum age. Only files this app wrote (`<date-time>/CHn*.mp4` directly under the
//! recording root) are ever deleted, and never recent ones.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sysinfo::Disks;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::layout::TIME_FORMAT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LowSpaceAction {
    /// Only tell the user.
    #[default]
    Warn,
    /// Delete the oldest recordings until there is room again.
    DeleteOldest,
    /// Finish the files cleanly and stop recording.
    Stop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Free space below this (GB) triggers `on_low`. 0 disables the check.
    pub min_free_gb: u32,
    pub on_low: LowSpaceAction,
    /// Delete recordings older than this many days. 0 = keep forever.
    pub max_age_days: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { min_free_gb: 10, on_low: LowSpaceAction::Warn, max_age_days: 0 }
    }
}

/// Never touch files younger than this: they may still be the segment that was just closed.
const PROTECT: Duration = Duration::from_secs(600);
const GB: u64 = 1024 * 1024 * 1024;

/// Free bytes on the volume that holds `path` (nearest existing parent).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() && p.pop() {}
    let p = p.canonicalize().ok()?;
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| p.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub len: u64,
}

fn is_recording_name(name: &str) -> bool {
    name.starts_with("CH") && name.ends_with(".mp4") && name[2..].chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Recording files written by this app under `root`, oldest first.
pub fn scan(root: &Path) -> Vec<Entry> {
    let mut out = vec![];
    let Ok(dirs) = std::fs::read_dir(root) else { return out };
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if NaiveDateTime::parse_from_str(&name, TIME_FORMAT).is_err() || !d.path().is_dir() {
            continue; // skips .recording, recovered, and anything foreign
        }
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            let Ok(md) = f.metadata() else { continue };
            if md.is_file() && is_recording_name(&fname) {
                out.push(Entry { path: f.path(), modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH), len: md.len() });
            }
        }
    }
    out.sort_by_key(|e| e.modified);
    out
}

/// Files older than `days` (oldest first), excluding protected recent ones.
pub fn pick_by_age(entries: &[Entry], now: SystemTime, days: u32) -> Vec<Entry> {
    let limit = Duration::from_secs(days as u64 * 86_400).max(PROTECT);
    entries
        .iter()
        .filter(|e| now.duration_since(e.modified).is_ok_and(|age| age > limit))
        .cloned()
        .collect()
}

/// Oldest files whose total size reaches `need` bytes (protected recent files are skipped).
pub fn pick_for_space(entries: &[Entry], now: SystemTime, need: u64) -> Vec<Entry> {
    let mut got = 0u64;
    let mut out = vec![];
    for e in entries {
        if got >= need {
            break;
        }
        if now.duration_since(e.modified).is_ok_and(|age| age > PROTECT) {
            got += e.len;
            out.push(e.clone());
        }
    }
    out
}

/// Delete the files and any date-time folder left empty. Returns (files, bytes).
pub fn delete(entries: &[Entry], root: &Path) -> (usize, u64) {
    let (mut n, mut bytes) = (0, 0);
    for e in entries {
        // Belt and braces: only ever delete inside the recording root.
        if !e.path.starts_with(root) {
            continue;
        }
        if std::fs::remove_file(&e.path).is_ok() {
            n += 1;
            bytes += e.len;
            if let Some(dir) = e.path.parent() {
                let _ = std::fs::remove_dir(dir); // only succeeds when empty
            }
        }
    }
    (n, bytes)
}

/// Check every 30 s. Adds human-readable lines to `notices`; may ask the recorder to stop.
pub fn spawn_monitor(
    cfg: StorageConfig,
    root: PathBuf,
    notices: Arc<Mutex<Vec<String>>>,
    stop: Arc<watch::Sender<bool>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let note = |msg: String| {
            let mut n = notices.lock().unwrap();
            if n.last() != Some(&msg) {
                n.push(msg);
                if n.len() > 20 {
                    n.remove(0);
                }
            }
        };
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            let (root2, cfg2) = (root.clone(), cfg);
            // File-system work off the async threads.
            let result = tokio::task::spawn_blocking(move || {
                let now = SystemTime::now();
                let mut msgs = vec![];
                let mut stop_now = false;
                if cfg2.max_age_days > 0 {
                    let old = pick_by_age(&scan(&root2), now, cfg2.max_age_days);
                    if !old.is_empty() {
                        let (n, b) = delete(&old, &root2);
                        msgs.push(format!("Deleted {n} recording(s) older than {} days ({:.1} GB)", cfg2.max_age_days, b as f64 / GB as f64));
                    }
                }
                if cfg2.min_free_gb > 0 {
                    if let Some(free) = free_space(&root2) {
                        let floor = cfg2.min_free_gb as u64 * GB;
                        if free < floor {
                            match cfg2.on_low {
                                LowSpaceAction::Warn => msgs.push(format!("Low disk space: {:.1} GB free", free as f64 / GB as f64)),
                                LowSpaceAction::Stop => {
                                    msgs.push(format!("Stopped: only {:.1} GB free (limit {} GB)", free as f64 / GB as f64, cfg2.min_free_gb));
                                    stop_now = true;
                                }
                                LowSpaceAction::DeleteOldest => {
                                    let need = floor + GB - free; // leave a 1 GB margin
                                    let victims = pick_for_space(&scan(&root2), now, need);
                                    let (n, b) = delete(&victims, &root2);
                                    if n > 0 {
                                        msgs.push(format!("Disk low: deleted {n} oldest recording(s) ({:.1} GB)", b as f64 / GB as f64));
                                    }
                                    if b < need {
                                        msgs.push("Disk low and nothing more can be deleted safely".into());
                                    }
                                }
                            }
                        }
                    }
                }
                (msgs, stop_now)
            })
            .await;
            if let Ok((msgs, stop_now)) = result {
                msgs.into_iter().for_each(&note);
                if stop_now {
                    let _ = stop.send(true);
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    fn make(root: &Path, dir: &str, file: &str, age_secs: u64, bytes: usize) -> PathBuf {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        let p = d.join(file);
        fs::write(&p, vec![0u8; bytes]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        p
    }

    #[test]
    fn scan_only_sees_our_files_oldest_first() {
        let r = tempfile::tempdir().unwrap();
        let new = make(r.path(), "2025-04-09_22-43-08", "CH1.mp4", 100, 10);
        let old = make(r.path(), "2025-04-09_22-13-08", "CH1.mp4", 5000, 10);
        make(r.path(), "2025-04-09_22-13-08", "notes.txt", 5000, 10);
        make(r.path(), ".recording", "CH1_p0_00000.mp4", 5000, 10);
        make(r.path(), "holiday", "CH1.mp4", 5000, 10);
        let got: Vec<_> = scan(r.path()).into_iter().map(|e| e.path).collect();
        assert_eq!(got, vec![old, new]);
    }

    #[test]
    fn space_pick_takes_oldest_first_and_spares_recent() {
        let now = SystemTime::now();
        let e = |name: &str, age: u64, len: u64| Entry { path: name.into(), modified: now - Duration::from_secs(age), len };
        let all = vec![e("a", 90_000, 100), e("b", 50_000, 100), e("c", 60, 100)];
        let got: Vec<_> = pick_for_space(&all, now, 150).into_iter().map(|e| e.path).collect();
        assert_eq!(got, vec![PathBuf::from("a"), PathBuf::from("b")]);
        // even a huge need never touches the file from a minute ago
        assert_eq!(pick_for_space(&all, now, 10_000).len(), 2);
    }

    #[test]
    fn age_pick_and_delete_remove_empty_folders() {
        let r = tempfile::tempdir().unwrap();
        make(r.path(), "2025-01-01_00-00-00", "CH1.mp4", 40 * 86_400, 10);
        let fresh = make(r.path(), "2025-04-09_22-43-08", "CH1.mp4", 100, 10);
        let victims = pick_by_age(&scan(r.path()), SystemTime::now(), 30);
        assert_eq!(victims.len(), 1);
        assert_eq!(delete(&victims, r.path()), (1, 10));
        assert!(!r.path().join("2025-01-01_00-00-00").exists(), "empty folder should go");
        assert!(fresh.exists());
    }

    #[test]
    fn delete_refuses_paths_outside_root() {
        let r = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let p = make(other.path(), "2025-01-01_00-00-00", "CH1.mp4", 90_000, 10);
        let e = Entry { path: p.clone(), modified: SystemTime::UNIX_EPOCH, len: 10 };
        assert_eq!(delete(&[e], r.path()), (0, 0));
        assert!(p.exists());
    }

    #[test]
    fn free_space_is_found_for_a_temp_dir() {
        assert!(free_space(&std::env::temp_dir().join("does/not/exist/yet")).is_some());
    }
}
