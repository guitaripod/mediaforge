use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::db::models::{ProbeResult, SubtitleStream};

#[derive(Clone)]
pub struct FFmpeg {
    ffmpeg_path: PathBuf,
    ffprobe_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    streams: Option<Vec<FfprobeStream>>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    index: i32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    bit_rate: Option<String>,
    channels: Option<i32>,
    #[serde(default)]
    tags: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    disposition: serde_json::Map<String, serde_json::Value>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
}

#[derive(Debug, Deserialize)]
struct SideData {
    side_data_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FfprobeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
}

impl FFmpeg {
    pub fn new(ffmpeg_path: PathBuf, ffprobe_path: PathBuf) -> Self {
        Self {
            ffmpeg_path,
            ffprobe_path,
        }
    }

    /// Probe a media file and return structured info
    pub async fn probe(&self, path: &Path) -> Result<ProbeResult> {
        let output = Command::new(&self.ffprobe_path)
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                "-show_format",
            ])
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run ffprobe")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffprobe failed: {}", stderr);
        }

        let probe: FfprobeOutput =
            serde_json::from_slice(&output.stdout).context("Failed to parse ffprobe output")?;

        let streams = probe.streams.unwrap_or_default();
        let format = probe.format;

        let mut result = ProbeResult {
            duration_secs: format
                .as_ref()
                .and_then(|f| f.duration.as_ref())
                .and_then(|d| d.parse::<f64>().ok()),
            video_codec: None,
            video_width: None,
            video_height: None,
            video_bitrate: None,
            hdr_format: None,
            audio_codec: None,
            audio_channels: None,
            audio_bitrate: None,
            subtitle_streams: Vec::new(),
        };

        for stream in &streams {
            match stream.codec_type.as_deref() {
                Some("video") => {
                    if result.video_codec.is_none() {
                        result.video_codec = stream.codec_name.clone();
                        result.video_width = stream.width;
                        result.video_height = stream.height;
                        result.video_bitrate =
                            stream.bit_rate.as_ref().and_then(|b| b.parse().ok());
                        result.hdr_format = detect_hdr(stream);
                    }
                }
                Some("audio") => {
                    if result.audio_codec.is_none() {
                        result.audio_codec = stream.codec_name.clone();
                        result.audio_channels = stream.channels;
                        result.audio_bitrate =
                            stream.bit_rate.as_ref().and_then(|b| b.parse().ok());
                    }
                }
                Some("subtitle") => {
                    let is_forced = stream
                        .disposition
                        .get("forced")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        == 1;
                    let is_default = stream
                        .disposition
                        .get("default")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        == 1;
                    let language = stream
                        .tags
                        .get("language")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    result.subtitle_streams.push(SubtitleStream {
                        index: stream.index,
                        codec: stream.codec_name.clone().unwrap_or_default(),
                        language,
                        is_forced,
                        is_default,
                    });
                }
                _ => {}
            }
        }

        Ok(result)
    }

    /// Generate HLS playlist and segments for a media file
    pub async fn generate_hls(
        &self,
        input_path: &Path,
        output_dir: &Path,
        segment_duration: u32,
        start_time: Option<f64>,
    ) -> Result<()> {
        std::fs::create_dir_all(output_dir)?;

        let playlist_path = output_dir.join("playlist.m3u8");
        let segment_pattern = output_dir.join("segment_%04d.ts");

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning"]);

        if let Some(start) = start_time {
            cmd.args(["-ss", &format!("{:.2}", start)]);
        }

        cmd.arg("-i").arg(input_path);

        // Video: copy if h264/h265, transcode otherwise
        // Audio: transcode to AAC for iOS compatibility
        cmd.args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            // Video settings - will be overridden per-file
            "-c:v",
            "copy",
            // Audio: always AAC for iOS
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ac",
            "2",
            // HLS settings
            "-f",
            "hls",
            "-hls_time",
            &segment_duration.to_string(),
            "-hls_list_size",
            "0",
            "-hls_segment_filename",
        ]);

        cmd.arg(&segment_pattern);
        cmd.arg(&playlist_path);

        debug!("Running HLS generation: {:?}", cmd);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run ffmpeg for HLS")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("FFmpeg HLS generation failed: {}", stderr);
        }

        Ok(())
    }

    /// Generate HLS with video transcoding (for non-iOS-compatible codecs)
    pub async fn generate_hls_transcode(
        &self,
        input_path: &Path,
        output_dir: &Path,
        segment_duration: u32,
        target_height: Option<i32>,
    ) -> Result<()> {
        std::fs::create_dir_all(output_dir)?;

        let playlist_path = output_dir.join("playlist.m3u8");
        let segment_pattern = output_dir.join("segment_%04d.ts");

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning"]);
        cmd.arg("-i").arg(input_path);

        cmd.args(["-map", "0:v:0", "-map", "0:a:0"]);

        // Video: transcode to h264 for maximum compatibility
        cmd.args(["-c:v", "libx264", "-preset", "fast", "-crf", "22"]);

        if let Some(height) = target_height {
            cmd.args([
                "-vf",
                &format!("scale=-2:{}", height),
            ]);
        }

        // Audio: AAC
        cmd.args(["-c:a", "aac", "-b:a", "192k", "-ac", "2"]);

        // HLS
        cmd.args([
            "-f",
            "hls",
            "-hls_time",
            &segment_duration.to_string(),
            "-hls_list_size",
            "0",
            "-hls_segment_filename",
        ]);
        cmd.arg(&segment_pattern);
        cmd.arg(&playlist_path);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run ffmpeg for HLS transcode")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("FFmpeg HLS transcode failed: {}", stderr);
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn remux_to_mp4(&self, input_path: &Path, output_path: &Path) -> Result<()> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let output = Command::new(&self.ffmpeg_path)
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "warning",
                "-i",
            ])
            .arg(input_path)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(output_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run ffmpeg for remux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("FFmpeg remux failed: {}", stderr);
        }

        Ok(())
    }

    /// Extract subtitle stream to VTT format
    pub async fn extract_subtitle_vtt(
        &self,
        input_path: &Path,
        stream_index: i32,
        output_path: &Path,
    ) -> Result<()> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let output = Command::new(&self.ffmpeg_path)
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "warning",
                "-i",
            ])
            .arg(input_path)
            .args(["-map", &format!("0:{}", stream_index), "-c:s", "webvtt"])
            .arg(output_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to extract subtitle")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Subtitle extraction failed (may be image-based): {}", stderr);
            anyhow::bail!("Subtitle extraction failed: {}", stderr);
        }

        Ok(())
    }

    /// Check if a video codec is natively playable on iOS
    pub fn is_ios_native_video(codec: &str) -> bool {
        matches!(
            codec.to_lowercase().as_str(),
            "h264" | "hevc" | "h265" | "vp9" | "av1"
        )
    }

    /// Check if an audio codec needs transcoding for iOS
    pub fn needs_audio_transcode(codec: &str) -> bool {
        matches!(
            codec.to_lowercase().as_str(),
            "dts" | "dts-hd" | "truehd" | "pcm_s16le" | "pcm_s24le" | "pcm_s32le"
        )
    }
}

fn detect_hdr(stream: &FfprobeStream) -> Option<String> {
    // Check for Dolby Vision
    for sd in &stream.side_data_list {
        if let Some(ref t) = sd.side_data_type
            && t.contains("Dolby Vision")
        {
            return Some("Dolby Vision".to_string());
        }
    }

    // Check for HDR10/HDR10+
    match (
        stream.color_transfer.as_deref(),
        stream.color_primaries.as_deref(),
    ) {
        (Some("smpte2084"), Some("bt2020")) => Some("HDR10".to_string()),
        (Some("arib-std-b67"), Some("bt2020")) => Some("HLG".to_string()),
        _ => None,
    }
}
