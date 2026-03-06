use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::api::ScanStatus;
use crate::db::Database;
use crate::ffmpeg::FFmpeg;
use crate::metadata::TmdbClient;
use crate::scanner::Scanner;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "m4v", "wmv", "flv", "ts", "webm",
];

const DEBOUNCE_SECS: u64 = 10;

fn is_video_event(paths: &[PathBuf]) -> bool {
    paths.iter().any(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
    })
}

pub async fn run(
    dirs: Vec<PathBuf>,
    db: Database,
    ffmpeg: FFmpeg,
    tmdb: TmdbClient,
    scan_status: Arc<ScanStatus>,
) {
    let (tx, mut rx) = mpsc::channel::<()>(16);

    let watcher_tx = tx.clone();
    let watch_dirs = dirs.clone();
    let _watcher = std::thread::spawn(move || {
        let rt_tx = watcher_tx;
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(
            move |res: Result<notify::Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        let dominated = matches!(
                            event.kind,
                            EventKind::Create(_)
                                | EventKind::Remove(_)
                                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        );
                        if dominated && is_video_event(&event.paths) {
                            debug!("File change detected: {:?}", event.paths);
                            let _ = rt_tx.try_send(());
                        }
                    }
                    Err(e) => error!("File watcher error: {}", e),
                }
            },
        ) {
            Ok(w) => w,
            Err(e) => {
                error!("Failed to create file watcher: {}", e);
                return;
            }
        };

        for dir in &watch_dirs {
            if dir.exists() {
                match watcher.watch(dir, RecursiveMode::Recursive) {
                    Ok(()) => info!("Watching directory: {}", dir.display()),
                    Err(e) => warn!("Failed to watch {}: {}", dir.display(), e),
                }
            }
        }

        std::thread::park();
    });

    info!("File watcher started for {} directories", dirs.len());

    loop {
        rx.recv().await;

        while rx.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_secs(DEBOUNCE_SECS)).await;
        while rx.try_recv().is_ok() {}

        if scan_status.is_running() {
            debug!("Skipping file-watch scan: scan already in progress");
            continue;
        }

        info!("File change detected, running library scan...");
        scan_status.start_scan();

        let scanner = Scanner::new(db.clone(), ffmpeg.clone());
        match scanner.scan_directories(&dirs).await {
            Ok(count) => {
                scan_status.set_items_found(count);
                if tmdb.has_key() {
                    scan_status.start_metadata();
                    if let Err(e) = tmdb.migrate_numeric_genres(&db).await {
                        error!("Genre migration failed: {}", e);
                    }
                    if let Err(e) = tmdb.update_movie_metadata(&db).await {
                        error!("Movie metadata update failed: {}", e);
                    }
                    if let Err(e) = tmdb.update_tv_metadata(&db).await {
                        error!("TV metadata update failed: {}", e);
                    }
                }
                scan_status.finish();
            }
            Err(e) => {
                error!("File-watch scan failed: {}", e);
                scan_status.fail(e.to_string());
            }
        }
    }
}
