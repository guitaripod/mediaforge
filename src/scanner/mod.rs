use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::db::models::{MediaItem, MediaType};
use crate::db::Database;
use crate::ffmpeg::FFmpeg;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "m4v", "wmv", "flv", "ts", "webm",
];

const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "vtt", "ass", "ssa", "sub", "idx"];

pub struct Scanner {
    db: Database,
    ffmpeg: FFmpeg,
}

impl Scanner {
    pub fn new(db: Database, ffmpeg: FFmpeg) -> Self {
        Self { db, ffmpeg }
    }

    /// Scan all configured media directories
    pub async fn scan_directories(&self, dirs: &[PathBuf]) -> Result<()> {
        for dir in dirs {
            if !dir.exists() {
                warn!("Media directory does not exist: {:?}", dir);
                continue;
            }
            info!("Scanning directory: {:?}", dir);
            self.scan_directory(dir).await?;
        }
        Ok(())
    }

    async fn scan_directory(&self, dir: &Path) -> Result<()> {
        let mut video_files: Vec<PathBuf> = Vec::new();
        let mut subtitle_files: Vec<PathBuf> = Vec::new();

        for entry in WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if VIDEO_EXTENSIONS.contains(&ext_lower.as_str()) {
                    video_files.push(path.to_path_buf());
                } else if SUBTITLE_EXTENSIONS.contains(&ext_lower.as_str()) {
                    subtitle_files.push(path.to_path_buf());
                }
            }
        }

        info!(
            "Found {} video files and {} subtitle files",
            video_files.len(),
            subtitle_files.len()
        );

        for video_path in &video_files {
            // Skip if already in database
            {
                let conn = self.db.conn();
                let path_str = video_path.to_string_lossy();
                let exists: bool = conn
                    .query_row(
                        "SELECT COUNT(*) FROM media_items WHERE file_path = ?1",
                        [path_str.as_ref()],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    > 0;
                if exists {
                    debug!("Skipping already indexed: {:?}", video_path);
                    continue;
                }
            }

            match self.index_video(video_path, &subtitle_files).await {
                Ok(_) => debug!("Indexed: {:?}", video_path),
                Err(e) => warn!("Failed to index {:?}: {}", video_path, e),
            }
        }

        Ok(())
    }

    async fn index_video(&self, path: &Path, subtitle_files: &[PathBuf]) -> Result<()> {
        let file_size = std::fs::metadata(path)?.len() as i64;
        let parsed = parse_filename(path);

        // Probe the file
        let probe = self.ffmpeg.probe(path).await?;

        let id = Uuid::new_v4().to_string();

        let item = MediaItem {
            id: id.clone(),
            title: parsed.title.clone(),
            sort_title: make_sort_title(&parsed.title),
            media_type: parsed.media_type.clone(),
            year: parsed.year,
            file_path: path.to_string_lossy().to_string(),
            file_size,
            duration_secs: probe.duration_secs,
            video_codec: probe.video_codec,
            video_width: probe.video_width,
            video_height: probe.video_height,
            video_bitrate: probe.video_bitrate,
            hdr_format: probe.hdr_format,
            audio_codec: probe.audio_codec,
            audio_channels: probe.audio_channels,
            audio_bitrate: probe.audio_bitrate,
            show_name: parsed.show_name.clone(),
            season_number: parsed.season_number,
            episode_number: parsed.episode_number,
            episode_title: parsed.episode_title,
            tmdb_id: None,
            overview: None,
            poster_path: None,
            backdrop_path: None,
            genres: None,
            rating: None,
            release_date: None,
            added_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        // Insert into database
        {
            let conn = self.db.conn();
            conn.execute(
                "INSERT OR REPLACE INTO media_items (
                    id, title, sort_title, media_type, year, file_path, file_size,
                    duration_secs, video_codec, video_width, video_height, video_bitrate,
                    hdr_format, audio_codec, audio_channels, audio_bitrate,
                    show_name, season_number, episode_number, episode_title,
                    tmdb_id, overview, poster_path, backdrop_path, genres, rating,
                    release_date, added_at, updated_at
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                    ?27, ?28, ?29
                )",
                rusqlite::params![
                    item.id,
                    item.title,
                    item.sort_title,
                    item.media_type.to_string(),
                    item.year,
                    item.file_path,
                    item.file_size,
                    item.duration_secs,
                    item.video_codec,
                    item.video_width,
                    item.video_height,
                    item.video_bitrate,
                    item.hdr_format,
                    item.audio_codec,
                    item.audio_channels,
                    item.audio_bitrate,
                    item.show_name,
                    item.season_number,
                    item.episode_number,
                    item.episode_title,
                    item.tmdb_id,
                    item.overview,
                    item.poster_path,
                    item.backdrop_path,
                    item.genres,
                    item.rating,
                    item.release_date,
                    item.added_at,
                    item.updated_at,
                ],
            )?;
        }

        // Index embedded subtitles
        for sub_stream in &probe.subtitle_streams {
            let sub_id = Uuid::new_v4().to_string();
            let conn = self.db.conn();
            conn.execute(
                "INSERT INTO subtitles (id, media_id, stream_index, language, codec, is_forced, is_default, is_external)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                rusqlite::params![
                    sub_id,
                    id,
                    sub_stream.index,
                    sub_stream.language,
                    sub_stream.codec,
                    sub_stream.is_forced as i32,
                    sub_stream.is_default as i32,
                ],
            )?;
        }

        // Find external subtitle files matching this video
        let video_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        for sub_path in subtitle_files {
            let sub_stem = sub_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            // Match if subtitle filename starts with video filename
            if sub_stem.starts_with(video_stem) {
                let language = extract_subtitle_language(sub_stem, video_stem);
                let codec = sub_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("srt")
                    .to_string();
                let sub_id = Uuid::new_v4().to_string();
                let conn = self.db.conn();
                conn.execute(
                    "INSERT INTO subtitles (id, media_id, file_path, language, codec, is_forced, is_default, is_external)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, 1)",
                    rusqlite::params![
                        sub_id,
                        id,
                        sub_path.to_string_lossy().to_string(),
                        language,
                        codec,
                    ],
                )?;
            }
        }

        // Ensure TV show entry exists
        if parsed.media_type == MediaType::Episode
            && let Some(ref show_name) = parsed.show_name
        {
            let conn = self.db.conn();
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM tv_shows WHERE name = ?1",
                    [show_name],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0;
            if !exists {
                let show_id = Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO tv_shows (id, name, added_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![show_id, show_name, chrono::Utc::now().to_rfc3339()],
                )?;
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
struct ParsedFilename {
    title: String,
    year: Option<i32>,
    media_type: MediaType,
    show_name: Option<String>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    episode_title: Option<String>,
}

fn parse_filename(path: &Path) -> ParsedFilename {
    let filename = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown");

    // Replace dots and underscores with spaces for parsing
    let clean = filename.replace(['.', '_'], " ");

    // Try to match TV show pattern: S01E02 or s01e02
    let tv_re = Regex::new(r"(?i)(.+?)\s*[Ss](\d{1,2})\s*[Ee](\d{1,3})(?:\s*(.+))?").unwrap();

    if let Some(caps) = tv_re.captures(&clean) {
        let show_name = caps[1].trim().to_string();
        let season: i32 = caps[2].parse().unwrap_or(1);
        let episode: i32 = caps[3].parse().unwrap_or(1);
        let episode_title = caps.get(4).map(|m| {
            // Clean up episode title - remove quality/codec info
            let raw = m.as_str().trim();
            clean_title_suffix(raw)
        });

        return ParsedFilename {
            title: format!("{} S{:02}E{:02}", show_name, season, episode),
            year: None,
            media_type: MediaType::Episode,
            show_name: Some(show_name),
            season_number: Some(season),
            episode_number: Some(episode),
            episode_title,
        };
    }

    // Movie: try to extract year
    let year_re = Regex::new(r"(.+?)\s*[\(\[]?(\d{4})[\)\]]?").unwrap();
    if let Some(caps) = year_re.captures(&clean) {
        let title = caps[1].trim().to_string();
        let year: i32 = caps[2].parse().unwrap_or(0);
        if (1900..=2035).contains(&year) {
            return ParsedFilename {
                title,
                year: Some(year),
                media_type: MediaType::Movie,
                show_name: None,
                season_number: None,
                episode_number: None,
                episode_title: None,
            };
        }
    }

    // Fallback: use the cleaned filename
    ParsedFilename {
        title: clean_title_suffix(&clean),
        year: None,
        media_type: MediaType::Movie,
        show_name: None,
        season_number: None,
        episode_number: None,
        episode_title: None,
    }
}

/// Remove common quality/codec suffixes from titles
fn clean_title_suffix(title: &str) -> String {
    let noise_re = Regex::new(
        r"(?i)\s*(1080p|2160p|720p|480p|4k|uhd|bluray|blu-ray|bdrip|brrip|web-dl|web|webrip|hdtv|dvdrip|remux|remastered|hdr|hdr10|dv|hevc|h265|h264|x264|x265|av1|aac|dts|dts-hd|truehd|atmos|flac|ac3|dd5|ddp5|10bit|8bit|amzn|nf|atvp|dsnp|hmax).*$"
    ).unwrap();
    noise_re.replace(title, "").trim().to_string()
}

fn make_sort_title(title: &str) -> String {
    let lower = title.to_lowercase();
    if let Some(rest) = lower.strip_prefix("the ") {
        rest.to_string()
    } else if let Some(rest) = lower.strip_prefix("a ") {
        rest.to_string()
    } else if let Some(rest) = lower.strip_prefix("an ") {
        rest.to_string()
    } else {
        lower
    }
}

fn extract_subtitle_language(sub_stem: &str, video_stem: &str) -> Option<String> {
    let suffix = &sub_stem[video_stem.len()..];
    let suffix = suffix.trim_start_matches(['.', '_', '-']);
    if suffix.is_empty() {
        None
    } else {
        // Common patterns: .en, .eng, .english, .en.forced
        let lang = suffix.split(['.', '_', '-']).next().unwrap_or(suffix);
        Some(lang.to_lowercase())
    }
}
