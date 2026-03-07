use anyhow::Result;
use dashmap::DashMap;
use std::path::{Path, PathBuf};
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

        if start_secs.is_none() {
            if let Some(session) = self.sessions.get(&session_key)
                && session.status == HlsStatus::Ready
            {
                return Ok(session.clone());
            }

            let output_dir = self.cache_dir.join("hls").join(&session_key);
            let master_path = output_dir.join("master.m3u8");
            if master_path.exists() {
                let session = HlsSession {
                    media_id: media_id.to_string(),
                    output_dir: output_dir.clone(),
                    needs_transcode: false,
                    status: HlsStatus::Ready,
                };
                self.sessions.insert(session_key, session.clone());
                return Ok(session);
            }
        }

        let output_dir = self.cache_dir.join("hls").join(&session_key);
        if start_secs.is_some() && output_dir.exists() {
            self.cancel_session(&session_key);
            std::fs::remove_dir_all(&output_dir).ok();
            self.sessions.remove(&session_key);
        }

        let needs_video_transcode = video_codec
            .map(|c| !FFmpeg::is_ios_native_video(c))
            .unwrap_or(true);

        let needs_audio_transcode = audio_codec
            .map(FFmpeg::needs_audio_transcode)
            .unwrap_or(true);

        let needs_transcode = needs_video_transcode || needs_audio_transcode;

        let session = HlsSession {
            media_id: media_id.to_string(),
            output_dir: output_dir.clone(),
            needs_transcode,
            status: HlsStatus::Preparing(0.0),
        };
        self.sessions
            .insert(session_key.clone(), session.clone());

        info!(
            "Preparing HLS stream for {} (audio={:?}, start={:?}): video_transcode={}, audio_transcode={}",
            media_id, audio_stream_index, start_secs, needs_video_transcode, needs_audio_transcode
        );

        if needs_video_transcode {
            let _permit = self.transcode_semaphore.acquire().await?;

            let input_path = Path::new(file_path);
            let sessions = self.sessions.clone();
            let sk = session_key.clone();
            let on_progress: ProgressCallback = Box::new(move |pct| {
                if let Some(mut session) = sessions.get_mut(&sk)
                    && matches!(session.status, HlsStatus::Preparing(_))
                {
                    session.status = HlsStatus::Preparing(pct);
                }
            });

            let cancel = CancellationToken::new();
            self.cancels.insert(session_key.clone(), cancel.clone());

            let result = self.ffmpeg
                .generate_hls_adaptive(AdaptiveHlsParams {
                    input_path: input_path.to_path_buf(),
                    output_dir: output_dir.clone(),
                    segment_duration: self.segment_duration,
                    source_height: source_height.unwrap_or(1080),
                    audio_stream_index,
                    duration_secs,
                    start_secs,
                    on_progress,
                    cancel: cancel.clone(),
                })
                .await;

            self.cancels.remove(&session_key);

            match result {
                Ok(()) => {
                    let session = HlsSession {
                        media_id: media_id.to_string(),
                        output_dir,
                        needs_transcode: true,
                        status: HlsStatus::Ready,
                    };
                    self.sessions.insert(session_key, session.clone());
                    info!("HLS stream ready for {}", media_id);
                    Ok(session)
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("cancelled") {
                        info!("HLS transcode cancelled for {}", media_id);
                    } else {
                        let session = HlsSession {
                            media_id: media_id.to_string(),
                            output_dir,
                            needs_transcode: true,
                            status: HlsStatus::Error(err_msg.clone()),
                        };
                        self.sessions.insert(session_key, session.clone());
                    }
                    Err(e)
                }
            }
        } else {
            let permit = self.transcode_semaphore.clone().acquire_owned().await?;

            let cancel = CancellationToken::new();
            self.cancels.insert(session_key.clone(), cancel.clone());

            std::fs::create_dir_all(&output_dir)?;
            let master = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=20000000\noriginal/playlist.m3u8\n";
            std::fs::write(output_dir.join("master.m3u8"), master)?;

            let ffmpeg = self.ffmpeg.clone();
            let sessions_bg = self.sessions.clone();
            let cancels_bg = self.cancels.clone();
            let sk_bg = session_key.clone();
            let media_id_bg = media_id.to_string();
            let input_owned = PathBuf::from(file_path);
            let output_dir_bg = output_dir.clone();
            let seg_dur = self.segment_duration;
            let cancel_bg = cancel.clone();

            tokio::spawn(async move {
                let _permit = permit;
                let result = ffmpeg
                    .generate_hls(RemuxHlsParams {
                        input_path: input_owned,
                        output_dir: output_dir_bg,
                        segment_duration: seg_dur,
                        start_secs,
                        transcode_audio: needs_audio_transcode,
                        audio_stream_index,
                        cancel: cancel_bg,
                    })
                    .await;

                cancels_bg.remove(&sk_bg);

                match result {
                    Ok(()) => {
                        info!("HLS remux completed for {}", media_id_bg);
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        if err_msg.contains("cancelled") {
                            info!("HLS remux cancelled for {}", media_id_bg);
                        } else {
                            error!("HLS remux failed for {}: {}", media_id_bg, err_msg);
                            if let Some(mut session) = sessions_bg.get_mut(&sk_bg) {
                                session.status = HlsStatus::Error(err_msg);
                            }
                        }
                    }
                }
            });

            let playlist_path = output_dir.join("original").join("playlist.m3u8");
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if cancel.is_cancelled() {
                    anyhow::bail!("Remux cancelled");
                }
                if playlist_path.exists() {
                    break;
                }
            }

            if !playlist_path.exists() {
                if let Some(session) = self.sessions.get(&session_key) {
                    if let HlsStatus::Error(ref msg) = session.status {
                        anyhow::bail!("{}", msg);
                    }
                }
                anyhow::bail!("HLS remux failed to produce playlist within timeout");
            }

            let session = HlsSession {
                media_id: media_id.to_string(),
                output_dir,
                needs_transcode: false,
                status: HlsStatus::Ready,
            };
            self.sessions.insert(session_key, session.clone());
            info!("HLS remux stream ready for {} (remux continuing in background)", media_id);
            Ok(session)
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
        let key = self.active.get(media_id)?;
        self.sessions.get(key.value())
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

    pub fn segment_path(&self, media_id: &str, variant: &str, segment_name: &str) -> Option<PathBuf> {
        let session = self.resolve(media_id)?;
        let path = session.output_dir.join(variant).join(segment_name);
        if path.exists() { Some(path) } else { None }
    }

    pub fn cleanup_session(&self, media_id: &str) -> Result<()> {
        self.cancel_media(media_id);
        let session_key = self.active.remove(media_id).map(|(_, k)| k);
        let key = session_key.as_deref().unwrap_or(media_id);
        if let Some((_, session)) = self.sessions.remove(key)
            && session.output_dir.exists()
        {
            std::fs::remove_dir_all(&session.output_dir)?;
            debug!("Cleaned up HLS cache for {}", key);
        }
        Ok(())
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
                let media_id = entry.file_name().to_string_lossy().to_string();
                self.cleanup_session(&media_id)?;
                info!("Cleaned up expired HLS session: {}", media_id);
                removed += 1;
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
        });

        mgr.active.insert("media1".into(), "media1_a3".into());
        mgr.sessions.insert("media1_a3".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/tmp/b"),
            needs_transcode: true,
            status: HlsStatus::Preparing(0.0),
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
        });
        mgr.active.insert("media1".into(), "media1".into());

        mgr.cancel_media("media1");
        assert!(token.is_cancelled());
        assert!(!output_dir.exists());
        assert!(mgr.sessions.get("media1").is_none());
    }

    #[test]
    fn cancel_session_without_output_dir_is_safe() {
        let mgr = test_manager();
        mgr.sessions.insert("media1".into(), HlsSession {
            media_id: "media1".into(),
            output_dir: PathBuf::from("/nonexistent/path"),
            needs_transcode: true,
            status: HlsStatus::Preparing(0.0),
        });
        mgr.active.insert("media1".into(), "media1".into());

        mgr.cancel_media("media1");
        assert!(mgr.sessions.get("media1").is_none());
    }
}
