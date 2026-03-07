use anyhow::Result;
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::ffmpeg::{AdaptiveHlsParams, FFmpeg, ProgressCallback, RemuxHlsParams};

pub struct PrepareStreamParams<'a> {
    pub media_id: &'a str,
    pub file_path: &'a str,
    pub video_codec: Option<&'a str>,
    pub audio_codec: Option<&'a str>,
    pub audio_stream_index: Option<i32>,
    pub source_height: Option<i32>,
    pub duration_secs: Option<f64>,
    pub start_secs: Option<f64>,
}

#[derive(Clone)]
pub struct HlsManager {
    ffmpeg: FFmpeg,
    cache_dir: PathBuf,
    segment_duration: u32,
    sessions: Arc<DashMap<String, HlsSession>>,
    active: Arc<DashMap<String, String>>,
    cancels: Arc<DashMap<String, CancellationToken>>,
    transcode_semaphore: Arc<Semaphore>,
}

#[derive(Debug, Clone)]
pub struct HlsSession {
    #[allow(dead_code)]
    pub media_id: String,
    pub output_dir: PathBuf,
    #[allow(dead_code)]
    pub needs_transcode: bool,
    pub status: HlsStatus,
    pub start_secs: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HlsStatus {
    Preparing(f32),
    Ready,
    Error(String),
}

impl HlsManager {
    pub fn new(
        ffmpeg: FFmpeg,
        cache_dir: PathBuf,
        segment_duration: u32,
        max_concurrent: usize,
    ) -> Self {
        Self {
            ffmpeg,
            cache_dir,
            segment_duration,
            sessions: Arc::new(DashMap::new()),
            active: Arc::new(DashMap::new()),
            cancels: Arc::new(DashMap::new()),
            transcode_semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    pub async fn prepare_stream(&self, params: PrepareStreamParams<'_>) -> Result<HlsSession> {
        let PrepareStreamParams {
            media_id, file_path, video_codec, audio_codec,
            audio_stream_index, source_height, duration_secs, start_secs,
        } = params;
        let session_key = match audio_stream_index {
            Some(idx) => format!("{}_a{}", media_id, idx),
            None => media_id.to_string(),
        };

        let prev_key = self.active.insert(media_id.to_string(), session_key.clone());
        if let Some(prev) = prev_key
            && prev != session_key
        {
            self.cancel_session(&prev);
        }

        let needs_video_transcode = video_codec
            .map(|c| !FFmpeg::is_ios_native_video(c))
            .unwrap_or(true);

        let needs_audio_transcode = audio_codec
            .map(FFmpeg::needs_audio_transcode)
            .unwrap_or(true);

        let needs_transcode = needs_video_transcode || needs_audio_transcode;

        if let Some(session) = self.sessions.get(&session_key)
            && session.status == HlsStatus::Ready
        {
            return Ok(session.clone());
        }

        {
            let output_dir = self.cache_dir.join("hls").join(&session_key);
            let master_path = output_dir.join("master.m3u8");
            if master_path.exists() {
                let variant_ready = output_dir.join("original").join("playlist.m3u8").exists()
                    || std::fs::read_dir(&output_dir)
                        .ok()
                        .map(|d| d.flatten().any(|e| e.path().join("playlist.m3u8").exists()))
                        .unwrap_or(false);
                if variant_ready {
                    let session = HlsSession {
                        media_id: media_id.to_string(),
                        output_dir: output_dir.clone(),
                        needs_transcode,
                        status: HlsStatus::Ready,
            start_secs: None,
                    };
                    self.sessions.insert(session_key, session.clone());
                    return Ok(session);
                }
            }
        }

        let output_dir = self.cache_dir.join("hls").join(&session_key);
        if start_secs.is_some() && needs_video_transcode && output_dir.exists() {
            self.cancel_session(&session_key);
            std::fs::remove_dir_all(&output_dir).ok();
            self.sessions.remove(&session_key);
        }

        let session = HlsSession {
            media_id: media_id.to_string(),
            output_dir: output_dir.clone(),
            needs_transcode,
            status: HlsStatus::Preparing(0.0),
            start_secs,
        };
        self.sessions
            .insert(session_key.clone(), session.clone());

        info!(
            "Preparing HLS stream for {} (audio={:?}, start={:?}): video_transcode={}, audio_transcode={}",
            media_id, audio_stream_index, start_secs, needs_video_transcode, needs_audio_transcode
        );

        if needs_video_transcode {
            let permit = self.transcode_semaphore.clone().acquire_owned().await?;
            let cancel = CancellationToken::new();
            self.cancels.insert(session_key.clone(), cancel.clone());

            let ffmpeg = self.ffmpeg.clone();
            let sessions = self.sessions.clone();
            let cancels = self.cancels.clone();
            let sk = session_key.clone();
            let mid = media_id.to_string();
            let od = output_dir.clone();
            let seg_dur = self.segment_duration;
            let fp = PathBuf::from(file_path);
            let src_height = source_height.unwrap_or(1080);
            let spawn_start_secs = start_secs;

            tokio::spawn(async move {
                let _permit = permit;
                let progress_sessions = sessions.clone();
                let progress_sk = sk.clone();
                let on_progress: ProgressCallback = Box::new(move |pct| {
                    if let Some(mut session) = progress_sessions.get_mut(&progress_sk)
                        && matches!(session.status, HlsStatus::Preparing(_))
                    {
                        session.status = HlsStatus::Preparing(pct);
                    }
                });

                let result = ffmpeg
                    .generate_hls_adaptive(AdaptiveHlsParams {
                        input_path: fp,
                        output_dir: od.clone(),
                        segment_duration: seg_dur,
                        source_height: src_height,
                        audio_stream_index,
                        duration_secs,
                        start_secs,
                        on_progress,
                        cancel: cancel.clone(),
                    })
                    .await;

                cancels.remove(&sk);

                match result {
                    Ok(()) => {
                        let session = HlsSession {
                            media_id: mid.clone(),
                            output_dir: od,
                            needs_transcode: true,
                            status: HlsStatus::Ready,
                            start_secs: spawn_start_secs,
                        };
                        sessions.insert(sk.clone(), session);
                        info!("HLS transcode complete for {}", mid);
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        if err_msg.contains("cancelled") {
                            info!("HLS transcode cancelled for {}", mid);
                        } else {
                            error!("HLS transcode failed for {}: {}", mid, err_msg);
                            let session = HlsSession {
                                media_id: mid,
                                output_dir: od,
                                needs_transcode: true,
                                status: HlsStatus::Error(err_msg),
                                start_secs: spawn_start_secs,
                            };
                            sessions.insert(sk, session);
                        }
                    }
                }
            });

            let master_path = output_dir.join("master.m3u8");
            for _ in 0..120 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if master_path.exists() {
                    let session = HlsSession {
                        media_id: media_id.to_string(),
                        output_dir,
                        needs_transcode: true,
                        status: HlsStatus::Ready,
                        start_secs,
                    };
                    self.sessions.insert(session_key, session.clone());
                    info!("HLS transcode started for {}, master playlist available", media_id);
                    return Ok(session);
                }
                if let Some(s) = self.sessions.get(&session_key) {
                    if let HlsStatus::Error(ref e) = s.status {
                        anyhow::bail!("HLS transcode failed: {}", e);
                    }
                }
            }

            anyhow::bail!("HLS transcode timed out waiting for master playlist")
        } else {
            std::fs::create_dir_all(&output_dir)?;
            let master = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=20000000\noriginal/playlist.m3u8\n";
            std::fs::write(output_dir.join("master.m3u8"), master)?;

            let permit = self.transcode_semaphore.clone().acquire_owned().await?;
            let cancel = CancellationToken::new();
            self.cancels.insert(session_key.clone(), cancel.clone());

            let ffmpeg = self.ffmpeg.clone();
            let sessions = self.sessions.clone();
            let cancels = self.cancels.clone();
            let sk = session_key.clone();
            let mid = media_id.to_string();
            let od = output_dir.clone();
            let seg_dur = self.segment_duration;
            let fp = PathBuf::from(file_path);

            tokio::spawn(async move {
                let _permit = permit;
                let result = ffmpeg
                    .generate_hls(RemuxHlsParams {
                        input_path: fp,
                        output_dir: od.clone(),
                        segment_duration: seg_dur,
                start_secs: None,
                        transcode_audio: needs_audio_transcode,
                        audio_stream_index,
                        cancel: cancel.clone(),
                    })
                    .await;

                cancels.remove(&sk);

                match result {
                    Ok(()) => {
                        let session = HlsSession {
                            media_id: mid.clone(),
                            output_dir: od,
                            needs_transcode: false,
                            status: HlsStatus::Ready,
            start_secs: None,
                        };
                        sessions.insert(sk.clone(), session);
                        info!("HLS remux complete for {}", mid);
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        if err_msg.contains("cancelled") {
                            info!("HLS remux cancelled for {}", mid);
                        } else {
                            error!("HLS remux failed for {}: {}", mid, err_msg);
                            let session = HlsSession {
                                media_id: mid,
                                output_dir: od,
                                needs_transcode: false,
                                status: HlsStatus::Error(err_msg),
            start_secs: None,
                            };
                            sessions.insert(sk, session);
                        }
                    }
                }
            });

            let playlist_path = output_dir.join("original").join("playlist.m3u8");
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if playlist_path.exists() {
                    let session = HlsSession {
                        media_id: media_id.to_string(),
                        output_dir,
                        needs_transcode: false,
                        status: HlsStatus::Ready,
            start_secs: None,
                    };
                    self.sessions.insert(session_key, session.clone());
                    info!("HLS remux started for {}, playlist available", media_id);
                    return Ok(session);
                }
                if let Some(s) = self.sessions.get(&session_key) {
                    if let HlsStatus::Error(ref e) = s.status {
                        anyhow::bail!("HLS remux failed: {}", e);
                    }
                }
            }

            anyhow::bail!("HLS remux timed out waiting for playlist")
        }
    }

    fn cancel_session(&self, session_key: &str) {
        if let Some((_, token)) = self.cancels.remove(session_key) {
            token.cancel();
            info!("Cancelled in-progress transcode for {}", session_key);
        }
        if let Some((_, session)) = self.sessions.remove(session_key)
            && session.output_dir.exists()
        {
            std::fs::remove_dir_all(&session.output_dir).ok();
            debug!("Removed cancelled session output for {}", session_key);
        }
    }

    pub fn cancel_media(&self, media_id: &str) {
        if let Some(key) = self.active.get(media_id) {
            self.cancel_session(key.value());
        }
    }

    fn resolve(&self, media_id: &str) -> Option<dashmap::mapref::one::Ref<'_, String, HlsSession>> {
        if let Some(key) = self.active.get(media_id) {
            if let Some(session) = self.sessions.get(key.value()) {
                return Some(session);
            }
        }

        self.recover_from_disk(media_id);
        let key = self.active.get(media_id)?;
        self.sessions.get(key.value())
    }

    fn recover_from_disk(&self, media_id: &str) {
        let hls_dir = self.cache_dir.join("hls");

        let dir = hls_dir.join(media_id);
        if dir.join("master.m3u8").exists() {
            debug!("Recovering HLS session from disk for {}", media_id);
            let session = HlsSession {
                media_id: media_id.to_string(),
                output_dir: dir,
                needs_transcode: false,
                status: HlsStatus::Ready,
            start_secs: None,
            };
            self.active.insert(media_id.to_string(), media_id.to_string());
            self.sessions.insert(media_id.to_string(), session);
            return;
        }

        let prefix = format!("{}_a", media_id);
        if let Ok(entries) = std::fs::read_dir(&hls_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(&prefix) && entry.path().join("master.m3u8").exists() {
                    debug!("Recovering HLS session from disk for {} (key={})", media_id, name);
                    let session = HlsSession {
                        media_id: media_id.to_string(),
                        output_dir: entry.path(),
                        needs_transcode: false,
                        status: HlsStatus::Ready,
            start_secs: None,
                    };
                    self.active.insert(media_id.to_string(), name.clone());
                    self.sessions.insert(name, session);
                    return;
                }
            }
        }
    }

    pub fn session_status(&self, media_id: &str) -> Option<HlsStatus> {
        self.resolve(media_id).map(|s| s.status.clone())
    }

    pub fn master_playlist_path(&self, media_id: &str) -> Option<PathBuf> {
        let session = self.resolve(media_id)?;
        let path = session.output_dir.join("master.m3u8");
        if path.exists() { Some(path) } else { None }
    }

    pub fn variant_playlist_path(&self, media_id: &str, variant: &str) -> Option<PathBuf> {
        let session = self.resolve(media_id)?;
        let path = session.output_dir.join(variant).join("playlist.m3u8");
        if path.exists() { Some(path) } else { None }
    }

    pub fn session_output_dir(&self, media_id: &str) -> Option<PathBuf> {
        self.resolve(media_id).map(|s| s.output_dir.clone())
    }

    pub fn session_needs_transcode(&self, media_id: &str) -> bool {
        self.resolve(media_id).map(|s| s.needs_transcode).unwrap_or(false)
    }

    pub fn session_start_secs(&self, media_id: &str) -> Option<f64> {
        self.resolve(media_id).and_then(|s| s.start_secs)
    }

    pub fn segment_path(&self, media_id: &str, variant: &str, segment_name: &str) -> Option<PathBuf> {
        let session = self.resolve(media_id)?;
        let path = session.output_dir.join(variant).join(segment_name);
        if path.exists() { Some(path) } else { None }
    }

    pub fn cleanup_expired(&self, max_age: std::time::Duration) -> Result<u64> {
        let hls_dir = self.cache_dir.join("hls");
        if !hls_dir.exists() {
            return Ok(0);
        }

        let mut removed = 0u64;
        for entry in std::fs::read_dir(&hls_dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if let Ok(modified) = metadata.modified()
                && let Ok(age) = modified.elapsed()
                && age > max_age
            {
                let dir_name = entry.file_name().to_string_lossy().to_string();

                if self.sessions.contains_key(&dir_name)
                    || self.active.iter().any(|r| *r.value() == dir_name)
                {
                    continue;
                }

                if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                    error!("Failed to remove expired HLS directory {}: {}", dir_name, e);
                } else {
                    info!("Cleaned up expired HLS directory: {}", dir_name);
                    removed += 1;
                }
            }
        }

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> HlsManager {
        HlsManager::new(
            FFmpeg::new("ffmpeg".into(), "ffprobe".into()),
            PathBuf::from("/tmp/test-hls"),
            6,
            2,
        )
    }

    #[test]
    fn active_session_tracks_latest_prepare() {
        let mgr = test_manager();

        mgr.active.insert("media1".into(), "media1".into());
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/tmp/test-hls/hls/media1"),
            needs_transcode: false,
            status: HlsStatus::Ready,
            start_secs: None,
        });

        assert_eq!(mgr.session_status("media1"), Some(HlsStatus::Ready));
        assert_eq!(mgr.session_status("nonexistent"), None);
    }

    #[test]
    fn active_session_switches_on_audio_track() {
        let mgr = test_manager();

        mgr.active.insert("media1".into(), "media1".into());
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/tmp/a"),
            needs_transcode: false,
            status: HlsStatus::Ready,
            start_secs: None,
        });

        mgr.active.insert("media1".into(), "media1_a3".into());
        mgr.sessions.insert("media1_a3".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/tmp/b"),
            needs_transcode: true,
            status: HlsStatus::Preparing(0.0),
            start_secs: None,
        });

        assert_eq!(mgr.session_status("media1"), Some(HlsStatus::Preparing(0.0)));
    }

    #[test]
    fn resolve_returns_none_without_active() {
        let mgr = test_manager();
        assert!(mgr.resolve("media1").is_none());
    }

    #[test]
    fn preparing_status_includes_progress() {
        let mgr = test_manager();
        mgr.active.insert("media1".into(), "media1".into());
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/tmp/test"),
            needs_transcode: true,
            status: HlsStatus::Preparing(0.0),
            start_secs: None,
        });

        assert_eq!(mgr.session_status("media1"), Some(HlsStatus::Preparing(0.0)));

        mgr.sessions.get_mut("media1").unwrap().status = HlsStatus::Preparing(42.5);
        assert_eq!(mgr.session_status("media1"), Some(HlsStatus::Preparing(42.5)));
    }

    #[test]
    fn cancel_media_cancels_token() {
        let mgr = test_manager();
        let token = CancellationToken::new();
        mgr.cancels.insert("media1".into(), token.clone());
        mgr.active.insert("media1".into(), "media1".into());

        assert!(!token.is_cancelled());
        mgr.cancel_media("media1");
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_media_noop_when_no_session() {
        let mgr = test_manager();
        mgr.cancel_media("nonexistent");
    }

    #[test]
    fn cancel_session_removes_output_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let output_dir = dir.path().join("hls").join("media1");
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(output_dir.join("master.m3u8"), "test").unwrap();

        let mgr = test_manager();
        let token = CancellationToken::new();
        mgr.cancels.insert("media1".into(), token.clone());
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: output_dir.clone(),
            needs_transcode: true,
            status: HlsStatus::Preparing(50.0),
            start_secs: None,
        });
        mgr.active.insert("media1".into(), "media1".into());

        mgr.cancel_media("media1");
        assert!(token.is_cancelled());
        assert!(!output_dir.exists());
        assert!(mgr.sessions.get("media1").is_none());
    }

    #[test]
    fn resolve_recovers_from_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = HlsManager::new(
            FFmpeg::new("ffmpeg".into(), "ffprobe".into()),
            dir.path().to_path_buf(),
            6,
            2,
        );

        let session_dir = dir.path().join("hls").join("media1");
        std::fs::create_dir_all(session_dir.join("original")).unwrap();
        std::fs::write(session_dir.join("master.m3u8"), "#EXTM3U\n").unwrap();
        std::fs::write(session_dir.join("original").join("playlist.m3u8"), "#EXTM3U\n").unwrap();

        assert!(mgr.sessions.is_empty());
        assert!(mgr.active.is_empty());

        let status = mgr.session_status("media1");
        assert_eq!(status, Some(HlsStatus::Ready));
        assert!(mgr.sessions.contains_key("media1"));
        assert!(mgr.active.contains_key("media1"));
    }

    #[test]
    fn cleanup_expired_skips_active_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = HlsManager::new(
            FFmpeg::new("ffmpeg".into(), "ffprobe".into()),
            dir.path().to_path_buf(),
            6,
            2,
        );

        let session_dir = dir.path().join("hls").join("media1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("master.m3u8"), "#EXTM3U\n").unwrap();

        mgr.active.insert("media1".into(), "media1".into());
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: session_dir.clone(),
            needs_transcode: false,
            status: HlsStatus::Ready,
            start_secs: None,
        });

        let removed = mgr.cleanup_expired(std::time::Duration::from_secs(0)).unwrap();
        assert_eq!(removed, 0);
        assert!(session_dir.exists());
    }

    #[test]
    fn cleanup_expired_removes_orphaned_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = HlsManager::new(
            FFmpeg::new("ffmpeg".into(), "ffprobe".into()),
            dir.path().to_path_buf(),
            6,
            2,
        );

        let orphan = dir.path().join("hls").join("old-session");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("master.m3u8"), "#EXTM3U\n").unwrap();

        let removed = mgr.cleanup_expired(std::time::Duration::from_secs(0)).unwrap();
        assert_eq!(removed, 1);
        assert!(!orphan.exists());
    }

    #[test]
    fn cancel_session_without_output_dir_is_safe() {
        let mgr = test_manager();
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/nonexistent/path"),
            needs_transcode: true,
            status: HlsStatus::Preparing(0.0),
            start_secs: None,
        });
        mgr.active.insert("media1".into(), "media1".into());

        mgr.cancel_media("media1");
        assert!(mgr.sessions.get("media1").is_none());
    }
}
