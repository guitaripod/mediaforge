use anyhow::{Context, Result};
use image::GenericImageView;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::db::models::{AudioStream, ProbeResult, SubtitleStream};

pub type ProgressCallback = Box<dyn Fn(f32) + Send + Sync>;

pub struct AdaptiveHlsParams {
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub segment_duration: u32,
    pub source_height: i32,
    pub audio_stream_index: Option<i32>,
    pub duration_secs: Option<f64>,
    pub start_secs: Option<f64>,
    pub on_progress: ProgressCallback,
    pub cancel: CancellationToken,
}

pub struct RemuxHlsParams {
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub segment_duration: u32,
    pub start_secs: Option<f64>,
    pub transcode_audio: bool,
    pub audio_stream_index: Option<i32>,
    pub cancel: CancellationToken,
}

struct Rendition {
    name: &'static str,
    height: i32,
    crf: u8,
    maxrate: &'static str,
    bufsize: &'static str,
    audio_bitrate: &'static str,
}

const RENDITIONS: &[Rendition] = &[
    Rendition { name: "720p", height: 720, crf: 23, maxrate: "2500k", bufsize: "5000k", audio_bitrate: "128k" },
    Rendition { name: "360p", height: 360, crf: 26, maxrate: "400k", bufsize: "800k", audio_bitrate: "64k" },
];

fn active_renditions(source_height: i32) -> Vec<&'static Rendition> {
    let active: Vec<&Rendition> = RENDITIONS.iter()
        .filter(|r| r.height < source_height)
        .collect();
    if active.is_empty() { vec![&RENDITIONS[1]] } else { active }
}

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
struct FfprobeFormat {
    duration: Option<String>,
    #[allow(dead_code)]
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
            audio_streams: Vec::new(),
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
                    let title = stream
                        .tags
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    result.audio_streams.push(AudioStream {
                        index: stream.index,
                        codec: stream.codec_name.clone().unwrap_or_default(),
                        language,
                        channels: stream.channels,
                        bitrate: stream.bit_rate.as_ref().and_then(|b| b.parse().ok()),
                        is_default,
                        title,
                    });
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

        if let Some(first_audio) = result.audio_streams.first() {
            result.audio_codec = Some(first_audio.codec.clone());
            result.audio_channels = first_audio.channels;
            result.audio_bitrate = first_audio.bitrate;
        }

        Ok(result)
    }

    pub async fn generate_hls(&self, params: RemuxHlsParams) -> Result<()> {
        let RemuxHlsParams {
            input_path, output_dir, segment_duration,
            start_secs, transcode_audio, audio_stream_index, cancel,
        } = params;

        let variant_dir = output_dir.join("original");
        std::fs::create_dir_all(&variant_dir)?;

        let playlist_path = variant_dir.join("playlist.m3u8");
        let segment_pattern = variant_dir.join("segment_%04d.ts");

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning"]);

        if let Some(start) = start_secs {
            cmd.args(["-ss", &format!("{:.2}", start)]);
        }

        let audio_map = match audio_stream_index {
            Some(idx) => format!("0:{}", idx),
            None => "0:a:0".to_string(),
        };

        cmd.arg("-i").arg(&input_path);
        cmd.args(["-map", "0:v:0", "-map", &audio_map, "-c:v", "copy"]);

        if transcode_audio {
            cmd.args(["-c:a", "aac", "-b:a", "192k", "-ac", "2"]);
        } else {
            cmd.args(["-c:a", "copy"]);
        }

        cmd.args([
            "-f",
            "hls",
            "-hls_time",
            &segment_duration.to_string(),
            "-hls_list_size",
            "0",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ]);

        cmd.arg(&segment_pattern);
        cmd.arg(&playlist_path);

        debug!("Running HLS remux: {:?}", cmd);

        let mut child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn ffmpeg for HLS")?;

        let stderr = child.stderr.take()
            .context("Failed to capture ffmpeg stderr")?;

        let cancelled = tokio::select! {
            result = async {
                let mut lines = BufReader::new(stderr).lines();
                let mut last = String::new();
                while let Some(line) = lines.next_line().await? {
                    last = line;
                }
                Ok::<_, anyhow::Error>(last)
            } => {
                result?;
                false
            }
            _ = cancel.cancelled() => true,
        };

        if cancelled {
            child.kill().await.ok();
            child.wait().await.ok();
            if output_dir.exists() {
                std::fs::remove_dir_all(output_dir).ok();
            }
            anyhow::bail!("Transcode cancelled");
        }

        let status = child.wait().await?;
        if !status.success() {
            anyhow::bail!("FFmpeg HLS generation failed");
        }

        let master = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=20000000\noriginal/playlist.m3u8\n";
        std::fs::write(output_dir.join("master.m3u8"), master)?;

        Ok(())
    }

    pub async fn generate_hls_adaptive(
        &self,
        params: AdaptiveHlsParams,
    ) -> Result<()> {
        let AdaptiveHlsParams {
            input_path, output_dir, segment_duration,
            source_height, audio_stream_index,
            duration_secs, start_secs, on_progress, cancel,
        } = params;

        let audio_map = match audio_stream_index {
            Some(idx) => format!("0:{}", idx),
            None => "0:a:0".to_string(),
        };

        let active = active_renditions(source_height);
        let skip_scale = source_height <= RENDITIONS.last().unwrap().height;

        for r in &active {
            std::fs::create_dir_all(output_dir.join(r.name))?;
        }

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning"]);

        if let Some(start) = start_secs {
            cmd.args(["-ss", &format!("{:.2}", start)]);
        }

        cmd.arg("-i").arg(&input_path);

        for _ in &active {
            cmd.args(["-map", "0:v:0", "-map", &audio_map]);
        }

        cmd.args(["-g", "48", "-keyint_min", "48", "-sc_threshold", "0"]);
        cmd.args(["-preset", "fast"]);

        for (i, r) in active.iter().enumerate() {
            cmd.arg(format!("-c:v:{}", i)).arg("libx264");
            cmd.arg(format!("-crf:v:{}", i)).arg(r.crf.to_string());
            cmd.arg(format!("-maxrate:v:{}", i)).arg(r.maxrate);
            cmd.arg(format!("-bufsize:v:{}", i)).arg(r.bufsize);
            if !skip_scale {
                cmd.arg(format!("-filter:v:{}", i)).arg(format!("scale=-2:{}", r.height));
            }
            cmd.arg(format!("-c:a:{}", i)).arg("aac");
            cmd.arg(format!("-b:a:{}", i)).arg(r.audio_bitrate);
            cmd.arg(format!("-ac:{}", i)).arg("2");
        }

        let var_map: String = active.iter().enumerate()
            .map(|(i, r)| format!("v:{},a:{},name:{}", i, i, r.name))
            .collect::<Vec<_>>()
            .join(" ");

        cmd.args(["-var_stream_map", &var_map]);

        cmd.args([
            "-f", "hls",
            "-hls_time", &segment_duration.to_string(),
            "-hls_list_size", "0",
            "-hls_playlist_type", "vod",
            "-hls_flags", "independent_segments",
            "-master_pl_name", "master.m3u8",
            "-hls_segment_filename",
        ]);

        cmd.arg(output_dir.join("%v").join("segment_%04d.ts"));
        cmd.arg(output_dir.join("%v").join("playlist.m3u8"));

        cmd.args(["-progress", "pipe:2"]);

        debug!("Running adaptive HLS transcode: {:?}", cmd);

        let mut child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn ffmpeg for adaptive HLS")?;

        let stderr = child.stderr.take()
            .context("Failed to capture ffmpeg stderr")?;
        let mut lines = BufReader::new(stderr).lines();
        let mut last_stderr = String::new();

        let effective_duration = match (duration_secs, start_secs) {
            (Some(d), Some(s)) => Some((d - s).max(0.0)),
            (Some(d), None) => Some(d),
            _ => None,
        };

        let cancelled = loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break false };
                    if let Some(duration) = effective_duration
                        && duration > 0.0
                        && let Some(time_str) = line.strip_prefix("out_time_us=")
                        && let Ok(us) = time_str.trim().parse::<i64>()
                    {
                        let secs = us as f64 / 1_000_000.0;
                        let pct = (secs / duration * 100.0).min(100.0) as f32;
                        on_progress(pct);
                    }
                    let is_progress_line = line.starts_with("out_time") || line.starts_with("frame=") || line.starts_with("progress=") || line.starts_with("bitrate=") || line.starts_with("total_size=") || line.starts_with("speed=") || line.starts_with("stream_") || line.starts_with("dup_frames=") || line.starts_with("drop_frames=") || line.starts_with("fps=");
                    if !is_progress_line {
                        last_stderr.push_str(&line);
                        last_stderr.push('\n');
                    }
                }
                _ = cancel.cancelled() => {
                    break true;
                }
            }
        };

        if cancelled {
            child.kill().await.ok();
            child.wait().await.ok();
            if output_dir.exists() {
                std::fs::remove_dir_all(&output_dir).ok();
            }
            anyhow::bail!("Transcode cancelled");
        }

        let status = child.wait().await?;
        if !status.success() {
            anyhow::bail!("FFmpeg adaptive HLS failed: {}", last_stderr.trim());
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

    pub async fn generate_sprites(
        &self,
        input_path: &Path,
        output_dir: &Path,
        duration_secs: f64,
    ) -> Result<SpriteResult> {
        std::fs::create_dir_all(output_dir)?;

        let interval = sprite_interval(duration_secs);
        let total_thumbs = (duration_secs / interval as f64).ceil() as u32;
        let cols = 10u32;
        let rows = (total_thumbs as f64 / cols as f64).ceil() as u32;

        let output_path = output_dir.join("sprites.jpg");

        let mut cmd = Command::new(&self.ffmpeg_path);
        cmd.args(["-y", "-hide_banner", "-loglevel", "warning"]);
        cmd.arg("-i").arg(input_path);
        cmd.args([
            "-vf",
            &format!("fps=1/{},scale=160:-1,tile={}x{}", interval, cols, rows),
            "-q:v", "5",
            "-frames:v", "1",
        ]);
        cmd.arg(&output_path);

        debug!("Running sprite generation: {:?}", cmd);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run ffmpeg for sprite generation")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("FFmpeg sprite generation failed: {}", stderr);
        }

        let thumb_width = 160u32;
        let thumb_height = {
            let (_, h) = image_dimensions(&output_path)?;
            h / rows.max(1)
        };

        let vtt = generate_sprite_vtt(duration_secs, interval, cols, thumb_width, thumb_height);
        std::fs::write(output_dir.join("sprites.vtt"), &vtt)?;

        Ok(SpriteResult { interval, cols, rows, thumb_width, thumb_height })
    }

    /// Check if a video codec can be direct-played
    pub fn is_ios_native_video(codec: &str) -> bool {
        matches!(
            codec.to_lowercase().as_str(),
            "h264" | "hevc" | "h265" | "vp9" | "av1"
        )
    }

    /// Check if an audio codec needs transcoding
    pub fn needs_audio_transcode(codec: &str) -> bool {
        matches!(
            codec.to_lowercase().as_str(),
            "dts" | "dts-hd" | "truehd" | "pcm_s16le" | "pcm_s24le" | "pcm_s32le"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_video_h264() {
        assert!(FFmpeg::is_ios_native_video("h264"));
    }

    #[test]
    fn native_video_hevc_variants() {
        assert!(FFmpeg::is_ios_native_video("hevc"));
        assert!(FFmpeg::is_ios_native_video("h265"));
    }

    #[test]
    fn native_video_case_insensitive() {
        assert!(FFmpeg::is_ios_native_video("H264"));
        assert!(FFmpeg::is_ios_native_video("HEVC"));
    }

    #[test]
    fn non_native_video() {
        assert!(!FFmpeg::is_ios_native_video("mpeg4"));
        assert!(!FFmpeg::is_ios_native_video("vc1"));
        assert!(!FFmpeg::is_ios_native_video("msmpeg4v3"));
    }

    #[test]
    fn audio_transcode_dts() {
        assert!(FFmpeg::needs_audio_transcode("dts"));
        assert!(FFmpeg::needs_audio_transcode("dts-hd"));
        assert!(FFmpeg::needs_audio_transcode("truehd"));
    }

    #[test]
    fn audio_transcode_pcm() {
        assert!(FFmpeg::needs_audio_transcode("pcm_s16le"));
        assert!(FFmpeg::needs_audio_transcode("pcm_s24le"));
        assert!(FFmpeg::needs_audio_transcode("pcm_s32le"));
    }

    #[test]
    fn audio_no_transcode() {
        assert!(!FFmpeg::needs_audio_transcode("aac"));
        assert!(!FFmpeg::needs_audio_transcode("ac3"));
        assert!(!FFmpeg::needs_audio_transcode("eac3"));
        assert!(!FFmpeg::needs_audio_transcode("flac"));
    }

    #[test]
    fn detect_hdr10() {
        let stream = FfprobeStream {
            index: 0,
            codec_type: Some("video".into()),
            codec_name: Some("hevc".into()),
            width: Some(3840),
            height: Some(2160),
            bit_rate: None,
            channels: None,
            tags: Default::default(),
            disposition: Default::default(),
            color_transfer: Some("smpte2084".into()),
            color_primaries: Some("bt2020".into()),
            side_data_list: vec![],
        };
        assert_eq!(detect_hdr(&stream), Some("HDR10".to_string()));
    }

    #[test]
    fn detect_hlg() {
        let stream = FfprobeStream {
            index: 0,
            codec_type: Some("video".into()),
            codec_name: Some("hevc".into()),
            width: None,
            height: None,
            bit_rate: None,
            channels: None,
            tags: Default::default(),
            disposition: Default::default(),
            color_transfer: Some("arib-std-b67".into()),
            color_primaries: Some("bt2020".into()),
            side_data_list: vec![],
        };
        assert_eq!(detect_hdr(&stream), Some("HLG".to_string()));
    }

    #[test]
    fn detect_dolby_vision() {
        let stream = FfprobeStream {
            index: 0,
            codec_type: Some("video".into()),
            codec_name: Some("hevc".into()),
            width: None,
            height: None,
            bit_rate: None,
            channels: None,
            tags: Default::default(),
            disposition: Default::default(),
            color_transfer: None,
            color_primaries: None,
            side_data_list: vec![SideData {
                side_data_type: Some("Dolby Vision Metadata".into()),
            }],
        };
        assert_eq!(detect_hdr(&stream), Some("Dolby Vision".to_string()));
    }

    #[test]
    fn detect_sdr() {
        let stream = FfprobeStream {
            index: 0,
            codec_type: Some("video".into()),
            codec_name: Some("h264".into()),
            width: None,
            height: None,
            bit_rate: None,
            channels: None,
            tags: Default::default(),
            disposition: Default::default(),
            color_transfer: None,
            color_primaries: None,
            side_data_list: vec![],
        };
        assert_eq!(detect_hdr(&stream), None);
    }

    fn rendition_names(height: i32) -> Vec<&'static str> {
        active_renditions(height).iter().map(|r| r.name).collect()
    }

    #[test]
    fn renditions_for_1080p() {
        assert_eq!(rendition_names(1080), vec!["720p", "360p"]);
    }

    #[test]
    fn renditions_for_4k() {
        assert_eq!(rendition_names(2160), vec!["720p", "360p"]);
    }

    #[test]
    fn renditions_for_720p() {
        assert_eq!(rendition_names(720), vec!["360p"]);
    }

    #[test]
    fn renditions_for_480p() {
        assert_eq!(rendition_names(480), vec!["360p"]);
    }

    #[test]
    fn renditions_for_360p_or_smaller() {
        assert_eq!(rendition_names(360), vec!["360p"]);
        assert_eq!(rendition_names(240), vec!["360p"]);
    }

    #[test]
    fn sprite_interval_short_video() {
        assert_eq!(sprite_interval(30.0), 2);
        assert_eq!(sprite_interval(59.9), 2);
    }

    #[test]
    fn sprite_interval_medium_video() {
        assert_eq!(sprite_interval(60.0), 5);
        assert_eq!(sprite_interval(300.0), 5);
        assert_eq!(sprite_interval(599.0), 5);
    }

    #[test]
    fn sprite_interval_long_video() {
        assert_eq!(sprite_interval(600.0), 10);
        assert_eq!(sprite_interval(7200.0), 10);
    }

    #[test]
    fn vtt_time_format() {
        assert_eq!(format_vtt_time(0.0), "00:00:00.000");
        assert_eq!(format_vtt_time(65.5), "00:01:05.500");
        assert_eq!(format_vtt_time(3661.123), "01:01:01.123");
    }

    #[test]
    fn sprite_vtt_basic() {
        let vtt = generate_sprite_vtt(25.0, 10, 10, 160, 90);
        assert!(vtt.starts_with("WEBVTT"));
        assert!(vtt.contains("sprites.jpg#xywh=0,0,160,90"));
        assert!(vtt.contains("sprites.jpg#xywh=160,0,160,90"));
        assert!(vtt.contains("sprites.jpg#xywh=320,0,160,90"));
        assert!(vtt.contains("00:00:00.000 --> 00:00:10.000"));
        assert!(vtt.contains("00:00:10.000 --> 00:00:20.000"));
        assert!(vtt.contains("00:00:20.000 --> 00:00:25.000"));
    }

    #[test]
    fn sprite_vtt_wraps_rows() {
        let vtt = generate_sprite_vtt(110.0, 10, 5, 160, 90);
        assert!(vtt.contains("sprites.jpg#xywh=0,0,160,90"));
        assert!(vtt.contains("sprites.jpg#xywh=640,0,160,90"));
        assert!(vtt.contains("sprites.jpg#xywh=0,90,160,90"));
    }
}

#[allow(dead_code)]
pub struct SpriteResult {
    pub interval: u32,
    pub cols: u32,
    pub rows: u32,
    pub thumb_width: u32,
    pub thumb_height: u32,
}

fn sprite_interval(duration_secs: f64) -> u32 {
    if duration_secs < 60.0 { 2 }
    else if duration_secs < 600.0 { 5 }
    else { 10 }
}

fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    let data = std::fs::read(path)?;
    let img = image::load_from_memory(&data).context("Failed to decode image")?;
    Ok(img.dimensions())
}

fn generate_sprite_vtt(duration: f64, interval: u32, cols: u32, tw: u32, th: u32) -> String {
    let mut vtt = String::from("WEBVTT\n\n");
    let total = (duration / interval as f64).ceil() as u32;
    for i in 0..total {
        let start = i as f64 * interval as f64;
        let end = ((i + 1) as f64 * interval as f64).min(duration);
        let col = i % cols;
        let row = i / cols;
        let x = col * tw;
        let y = row * th;

        vtt.push_str(&format!(
            "{} --> {}\nsprites.jpg#xywh={},{},{},{}\n\n",
            format_vtt_time(start),
            format_vtt_time(end),
            x, y, tw, th
        ));
    }
    vtt
}

fn format_vtt_time(secs: f64) -> String {
    let h = (secs / 3600.0) as u32;
    let m = ((secs % 3600.0) / 60.0) as u32;
    let s = secs % 60.0;
    format!("{:02}:{:02}:{:06.3}", h, m, s)
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
