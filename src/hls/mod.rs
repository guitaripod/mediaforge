use anyhow::Result;
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, info};

use crate::ffmpeg::FFmpeg;

/// Manages HLS session creation, caching, and cleanup
#[derive(Clone)]
pub struct HlsManager {
    ffmpeg: FFmpeg,
    cache_dir: PathBuf,
    segment_duration: u32,
    sessions: Arc<DashMap<String, HlsSession>>,
    active: Arc<DashMap<String, String>>,
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
    Preparing,
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
            transcode_semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    pub async fn prepare_stream(
        &self,
        media_id: &str,
        file_path: &str,
        video_codec: Option<&str>,
        audio_codec: Option<&str>,
        audio_stream_index: Option<i32>,
        source_height: Option<i32>,
    ) -> Result<HlsSession> {
        let session_key = match audio_stream_index {
            Some(idx) => format!("{}_a{}", media_id, idx),
            None => media_id.to_string(),
        };

        self.active.insert(media_id.to_string(), session_key.clone());

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
            self.sessions
                .insert(session_key, session.clone());
            return Ok(session);
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
            status: HlsStatus::Preparing,
        };
        self.sessions
            .insert(session_key.clone(), session.clone());

        info!(
            "Preparing HLS stream for {} (audio={:?}): video_transcode={}, audio_transcode={}",
            media_id, audio_stream_index, needs_video_transcode, needs_audio_transcode
        );

        let _permit = self.transcode_semaphore.acquire().await?;

        let input_path = Path::new(file_path);

        let result = if needs_video_transcode {
            let height = source_height.unwrap_or(1080);
            self.ffmpeg
                .generate_hls_adaptive(input_path, &output_dir, self.segment_duration, height, audio_stream_index)
                .await
        } else {
            self.ffmpeg
                .generate_hls(input_path, &output_dir, self.segment_duration, None, needs_audio_transcode, audio_stream_index)
                .await
        };

        match result {
            Ok(()) => {
                let session = HlsSession {
                    media_id: media_id.to_string(),
                    output_dir,
                    needs_transcode,
                    status: HlsStatus::Ready,
                };
                self.sessions
                    .insert(session_key, session.clone());
                info!("HLS stream ready for {}", media_id);
                Ok(session)
            }
            Err(e) => {
                let err_msg = e.to_string();
                let session = HlsSession {
                    media_id: media_id.to_string(),
                    output_dir,
                    needs_transcode,
                    status: HlsStatus::Error(err_msg.clone()),
                };
                self.sessions
                    .insert(session_key, session.clone());
                Err(e)
            }
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

    /// Clean up HLS cache for a specific media item
    pub fn cleanup_session(&self, media_id: &str) -> Result<()> {
        if let Some((_, session)) = self.sessions.remove(media_id)
            && session.output_dir.exists()
        {
            std::fs::remove_dir_all(&session.output_dir)?;
            debug!("Cleaned up HLS cache for {}", media_id);
        }
        Ok(())
    }

    /// Clean up all expired HLS sessions (older than max_age)
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
            status: HlsStatus::Preparing,
        });

        assert_eq!(mgr.session_status("media1"), Some(HlsStatus::Preparing));
    }

    #[test]
    fn resolve_returns_none_without_active() {
        let mgr = test_manager();
        assert!(mgr.resolve("media1").is_none());
    }
}
