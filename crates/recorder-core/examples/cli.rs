//! Dev CLI: `cargo run --example cli -- <root> <seconds> <segment_secs> [new|same] [device ...]`
//! No device names => one synthetic test-pattern channel, so it runs without hardware.
use std::time::Duration;

use recorder_core::{ChannelConfig, RecordConfig, Recorder, Source, SplitLayout};

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let root = a.first().cloned().unwrap_or_else(|| "recordings".into());
    let secs: u64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
    let seg: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let layout = match a.get(3).map(String::as_str) {
        Some("same") => SplitLayout::SameFolder,
        _ => SplitLayout::NewFolder,
    };
    let devices: Vec<_> = a.iter().skip(4).cloned().collect();
    let sources: Vec<Source> = if devices.is_empty() {
        let n: usize = std::env::var("CHANNELS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        vec![Source::TestPattern; n]
    } else {
        devices.into_iter().map(Source::Device).collect()
    };

    let cfg = RecordConfig {
        root: root.into(),
        segment_secs: seg,
        split_layout: layout,
        channels: sources
            .into_iter()
            .enumerate()
            .map(|(i, source)| ChannelConfig {
                id: i as u32 + 1,
                source,
                size: std::env::var("SIZE").ok(),
                fps: std::env::var("FPS").ok().and_then(|v| v.parse().ok()).unwrap_or(30),
                deinterlace: match std::env::var("DEINTERLACE").as_deref() {
                    Ok(_) => recorder_core::Deinterlace::On,
                    Err(_) => recorder_core::Deinterlace::Off,
                },
                detected_size: None,
                detected_format: None,
            })
            .collect(),
        ffmpeg_path: std::env::var_os("FFMPEG").map(Into::into).unwrap_or_else(|| "ffmpeg".into()),
        preview_dir: std::env::var_os("PREVIEW_DIR").map(Into::into),
        storage: recorder_core::storage::StorageConfig {
            min_free_gb: std::env::var("STORAGE_MIN_FREE_GB").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            on_low: match std::env::var("STORAGE_ACTION").as_deref() {
                Ok("stop") => recorder_core::storage::LowSpaceAction::Stop,
                Ok("delete") => recorder_core::storage::LowSpaceAction::DeleteOldest,
                _ => recorder_core::storage::LowSpaceAction::Warn,
            },
            max_age_days: std::env::var("STORAGE_MAX_AGE").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
        },
        preview_only: std::env::var_os("MONITOR").is_some(),
        overlay: recorder_core::overlay::OverlayConfig {
            enabled: std::env::var_os("OVERLAY").is_some(),
            font_file: std::env::var_os("OVERLAY_FONT").map(Into::into),
            mode: if std::env::var("OVERLAY_MODE").as_deref() == Ok("burn") {
                recorder_core::overlay::OverlayMode::Burn
            } else {
                recorder_core::overlay::OverlayMode::Layer
            },
            items: {
                // OVERLAY_TEXT=1 keeps the two default text items; OVERLAY_IMAGE adds a picture.
                let mut v = if std::env::var_os("OVERLAY_TEXT").is_some() {
                    recorder_core::overlay::OverlayConfig::default().items
                } else {
                    vec![]
                };
                if std::env::var_os("OVERLAY_CLOCKMS").is_some() {
                    v.push(recorder_core::overlay::OverlayItem { text: "{clock_ms}".into(), x: 0.3, y: 0.4, size: 0.1, bg_opacity: 0.7, ..Default::default() });
                }
                if std::env::var_os("OVERLAY_THAI").is_some() {
                    v.push(recorder_core::overlay::OverlayItem {
                        text: "\u{e17}\u{e14}\u{e2a}\u{e2d}\u{e1a}\u{e20}\u{e32}\u{e29}\u{e32}\u{e44}\u{e17}\u{e22} \u{e19}\u{e49}\u{e33}\u{e25}\u{e36}\u{e01} {depth} \u{e40}\u{e21}\u{e15}\u{e23}".into(),
                        x: 0.3,
                        y: 0.45,
                        size: 0.07,
                        bg_opacity: 0.6,
                        ..Default::default()
                    });
                }
                if let Ok(p) = std::env::var("OVERLAY_IMAGE") {
                    v.push(recorder_core::overlay::OverlayItem {
                        kind: recorder_core::overlay::ItemKind::Image,
                        image: Some(p.into()),
                        x: 0.7,
                        y: 0.1,
                        width: 0.2,
                        ..Default::default()
                    });
                }
                v
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = cfg;
    let mut watcher = None;
    if let Some(dump) = std::env::var_os("STREAM") {
        // Subscribe to the raw preview like the app does; report rate and dump the first frame.
        let hub = std::sync::Arc::new(recorder_core::preview::PreviewHub::default());
        let (mut rx, _) = hub.subscribe(1);
        cfg.preview_hub = Some(hub);
        watcher = Some(tokio::spawn(async move {
            let (mut n, mut first, t0) = (0u32, true, std::time::Instant::now());
            let mut last = std::time::Instant::now();
            let mut gaps: Vec<f64> = vec![];
            loop {
                let f = match rx.recv().await {
                    Ok(f) => f,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                n += 1;
                gaps.push(last.elapsed().as_secs_f64() * 1000.0);
                last = std::time::Instant::now();
                if first {
                    first = false;
                    let w = u32::from_le_bytes(f[0..4].try_into().unwrap());
                    let h = u32::from_le_bytes(f[4..8].try_into().unwrap());
                    println!("PREVIEW first frame {w}x{h}, {} bytes", f.len());
                    std::fs::write(&dump, &f[recorder_core::preview::HEADER_LEN..]).ok();
                }
                if n % 120 == 0 {
                    let mut g = gaps[gaps.len() - 119..].to_vec();
                    g.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    println!("PREVIEW {n} frames, {:.1} fps overall; last 120 gaps p50={:.0}ms p95={:.0}ms max={:.0}ms", n as f64 / t0.elapsed().as_secs_f64(), g[g.len() / 2], g[g.len() * 95 / 100], g[g.len() - 1]);
                }
            }
        }));
    }
    let live_cfg = cfg.overlay.clone();
    let rec = Recorder::start(cfg).await.expect("start");
    println!("SESSION_START_EPOCH_MS={}", rec.session_start.timestamp_millis());
    println!("encoder: {:?} warnings: {:?}", rec.encoder, rec.warnings);
    for i in 0..secs / 2 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if i == 4 && std::env::var_os("LIVE_EDIT").is_some() {
            // Move/restyle the first item and add a new one while recording.
            let mut o = live_cfg.clone();
            if let Some(it) = o.items.first_mut() {
                it.x = 0.5;
                it.y = 0.5;
                it.color = "#FFFF00".into();
                it.size = 0.09;
                it.bold = true;
            }
            o.items.push(recorder_core::overlay::OverlayItem { text: "ADDED LIVE \u{e17}\u{e14}\u{e2a}\u{e2d}\u{e1a}".into(), x: 0.05, y: 0.8, bg_opacity: 0.6, ..Default::default() });
            rec.update_overlay(o);
            println!("-- overlay edited live --");
        }
        for s in rec.status() {
            println!(
                "CH{} {:?} frames={} dup={} drop={} speed={:.2} cpu={:.1}% rss={:.0}MB segs={} err={:?}",
                s.id, s.state, s.frames, s.duplicated, s.dropped, s.speed, s.cpu_percent, s.rss_mb, s.segments, s.last_error
            );
        }
    }
    let notes = rec.notices();
    rec.stop().await;
    for n in notes {
        println!("NOTICE: {n}");
    }
    if let Some(w) = watcher {
        w.abort();
    }
    println!("stopped");
}
