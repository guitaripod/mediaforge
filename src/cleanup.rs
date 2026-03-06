use std::path::Path;
use std::time::Duration;

use tracing::{debug, error, info};

use crate::config::CleanupConfig;
use crate::db::Database;
use crate::hls::HlsManager;

pub async fn run(config: CleanupConfig, hls: HlsManager, db: Database, cache_dir: std::path::PathBuf) {
    let interval = Duration::from_secs(config.interval_secs);
    let mut timer = tokio::time::interval(interval);
    timer.tick().await;

    loop {
        timer.tick().await;
        debug!("Running scheduled cleanup");

        let mut total = 0u64;

        match hls.cleanup_expired(Duration::from_secs(config.hls_max_age_secs)) {
            Ok(n) => total += n,
            Err(e) => error!("HLS cleanup failed: {}", e),
        }

        total += cleanup_file_cache(
            &cache_dir.join("subs"),
            Duration::from_secs(config.subtitle_max_age_secs),
            "subtitle",
        );

        total += cleanup_file_cache(
            &cache_dir.join("images"),
            Duration::from_secs(config.image_max_age_secs),
            "image",
        );

        let conn = db.conn();
        let retention = format!("-{} days", config.activity_retention_days);
        match conn.execute(
            "DELETE FROM activity_log WHERE created_at < datetime('now', ?1)",
            [&retention],
        ) {
            Ok(n) if n > 0 => {
                info!("Pruned {} old activity log entries", n);
                total += n as u64;
            }
            Err(e) => error!("Activity log cleanup failed: {}", e),
            _ => {}
        }

        if total > 0 {
            info!("Cleanup complete: {} items removed", total);
        }
    }
}

fn cleanup_file_cache(dir: &Path, max_age: Duration, label: &str) -> u64 {
    if !dir.exists() {
        return 0;
    }

    let mut removed = 0u64;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            error!("Failed to read {} cache dir: {}", label, e);
            return 0;
        }
    };

    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        if metadata.is_dir() {
            if let Err(e) = cleanup_image_size_dir(&entry.path(), max_age, &mut removed) {
                error!("Failed to clean {} subdir: {}", label, e);
            }
            continue;
        }

        let expired = metadata
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);

        if expired {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                error!("Failed to remove expired {} file: {}", label, e);
            } else {
                removed += 1;
            }
        }
    }

    if removed > 0 {
        info!("Cleaned up {} expired {} cache files", removed, label);
    }
    removed
}

fn cleanup_image_size_dir(dir: &Path, max_age: Duration, removed: &mut u64) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }

        let expired = metadata
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);

        if expired {
            std::fs::remove_file(entry.path())?;
            *removed += 1;
        }
    }
    Ok(())
}
