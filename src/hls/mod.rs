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
    /// Maps media_id -> HLS session state
    sessions: Arc<DashMap<String, HlsSession>>,
    /// Limit concurrent transcodes
    transcode_semaphore: Arc<Semaphore>,
}

#[derive(Debug, Clone)]
pub struct HlsSession {
    #[allow(dead_code)]
    pub media_id: String,
    pub output_dir: PathBuf,
    pub playlist_path: PathBuf,
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
            transcode_semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// Prepare an HLS stream for a media item
    pub async fn prepare_stream(
        &self,
        media_id: &str,
        file_path: &str,
        video_codec: Option<&str>,
        audio_codec: Option<&str>,
        audio_stream_index: Option<i32>,
    ) -> Result<HlsSession> {
        let session_key = match audio_stream_index {
            Some(idx) => format!("{}_a{}", media_id, idx),
            None => media_id.to_string(),
        };

        if let Some(session) = self.sessions.get(&session_key)
            && session.status == HlsStatus::Ready
        {
            return Ok(session.clone());
        }

        let output_dir = self.cache_dir.join("hls").join(&session_key);
        let playlist_path = output_dir.join("playlist.m3u8");

        if playlist_path.exists() {
            let session = HlsSession {
                media_id: media_id.to_string(),
                output_dir: output_dir.clone(),
                playlist_path: playlist_path.clone(),
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
            playlist_path: playlist_path.clone(),
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
            self.ffmpeg
                .generate_hls_transcode(input_path, &output_dir, self.segment_duration, None, audio_stream_index)
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
                    playlist_path,
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
                    playlist_path,
                    needs_transcode,
                    status: HlsStatus::Error(err_msg.clone()),
                };
                self.sessions
                    .insert(session_key, session.clone());
                Err(e)
            }
        }
    }

    /// Get the path to a specific HLS segment
    pub fn segment_path(&self, media_id: &str, segment_name: &str) -> Option<PathBuf> {
        let session = self.sessions.get(media_id)?;
        let path = session.output_dir.join(segment_name);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// Get the playlist path for a media item
    pub fn playlist_path(&self, media_id: &str) -> Option<PathBuf> {
        let session = self.sessions.get(media_id)?;
        if session.playlist_path.exists() {
            Some(session.playlist_path.clone())
        } else {
            None
        }
    }

    /// Get session status
    pub fn session_status(&self, media_id: &str) -> Option<HlsStatus> {
        self.sessions.get(media_id).map(|s| s.status.clone())
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
    pub fn cleanup_expired(&self, max_age: std::time::Duration) -> Result<()> {
        let hls_dir = self.cache_dir.join("hls");
        if !hls_dir.exists() {
            return Ok(());
        }

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
            }
        }

        Ok(())
    }
}
