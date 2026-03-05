use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
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

static TV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(.+?)\s*[Ss](\d{1,2})\s*[Ee](\d{1,3})(?:\s*(.+))?").unwrap()
});

static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\(\[]?(\d{4})[\)\]]?").unwrap()
});

static NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*(1080p|2160p|720p|480p|4k|uhd|bluray|blu-ray|bdrip|brrip|web-dl|web|webrip|hdtv|dvdrip|remux|remastered|hdr|hdr10|dv|hevc|h265|h264|x264|x265|av1|aac|dts|dts-hd|truehd|atmos|flac|ac3|dd5|ddp5|10bit|8bit|amzn|nf|atvp|dsnp|hmax).*$"
    ).unwrap()
});

fn is_macos_resource_fork(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("._"))
}

pub struct Scanner {
    db: Database,
    ffmpeg: FFmpeg,
}

impl Scanner {
    pub fn new(db: Database, ffmpeg: FFmpeg) -> Self {
        Self { db, ffmpeg }
    }

    pub async fn scan_directories(&self, dirs: &[PathBuf]) -> Result<()> {
        for dir in dirs {
            if !dir.exists() {
                warn!("Media directory does not exist: {:?}", dir);
                continue;
            }
            info!("Scanning directory: {:?}", dir);
            self.scan_directory(dir).await?;
        }
        self.prune_stale_entries()?;
        Ok(())
    }

    fn prune_stale_entries(&self) -> Result<()> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT id, file_path FROM media_items")?;
        let stale: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(_, path): &(String, String)| !Path::new(path).exists())
            .collect();

        if stale.is_empty() {
            return Ok(());
        }

        info!("Pruning {} stale entries (files no longer on disk)", stale.len());
        for (id, path) in &stale {
            conn.execute("DELETE FROM media_items WHERE id = ?1", [id])?;
            debug!("Pruned stale entry: {}", path);
        }

        let orphan_shows = conn.execute(
            "DELETE FROM tv_shows WHERE name NOT IN (SELECT DISTINCT show_name FROM media_items WHERE show_name IS NOT NULL)",
            [],
        )?;
        if orphan_shows > 0 {
            info!("Pruned {} orphan TV show entries", orphan_shows);
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
            if !path.is_file() || is_macos_resource_fork(path) {
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

        for sub_stream in &probe.subtitle_streams {
            let sub_id = Uuid::new_v4().to_string();
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

        let video_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        for sub_path in subtitle_files {
            let sub_stem = sub_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if sub_stem.starts_with(video_stem) {
                let language = extract_subtitle_language(sub_stem, video_stem);
                let codec = sub_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("srt")
                    .to_string();
                let sub_id = Uuid::new_v4().to_string();
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

        if parsed.media_type == MediaType::Episode
            && let Some(ref show_name) = parsed.show_name
        {
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

    let clean = filename.replace(['.', '_'], " ");

    if let Some(caps) = TV_RE.captures(&clean) {
        let show_name = caps[1].trim().to_string();
        let season: i32 = caps[2].parse().unwrap_or(1);
        let episode: i32 = caps[3].parse().unwrap_or(1);
        let episode_title = caps.get(4).map(|m| {
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

    let mut last_year: Option<(usize, i32)> = None;
    for m in YEAR_RE.find_iter(&clean) {
        let digits = m.as_str().trim_matches(|c| c == '(' || c == ')' || c == '[' || c == ']');
        if let Ok(y) = digits.parse::<i32>() {
            if (1900..=2035).contains(&y) {
                last_year = Some((m.start(), y));
            }
        }
    }
    if let Some((pos, year)) = last_year {
        let title = clean[..pos].trim().to_string();
        let title = clean_title_suffix(&title);
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

fn clean_title_suffix(title: &str) -> String {
    NOISE_RE.replace(title, "").trim().to_string()
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
        let lang = suffix.split(['.', '_', '-']).next().unwrap_or(suffix);
        Some(lang.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_movie_with_year() {
        let p = parse_filename(Path::new("/movies/The Matrix (1999).mkv"));
        assert_eq!(p.title, "The Matrix");
        assert_eq!(p.year, Some(1999));
        assert_eq!(p.media_type, MediaType::Movie);
        assert!(p.show_name.is_none());
    }

    #[test]
    fn parse_movie_with_year_no_parens() {
        let p = parse_filename(Path::new("/movies/Blade.Runner.2049.2017.2160p.UHD.mkv"));
        assert_eq!(p.title, "Blade Runner 2049");
        assert_eq!(p.year, Some(2017));
        assert_eq!(p.media_type, MediaType::Movie);
    }

    #[test]
    fn parse_movie_no_year() {
        let p = parse_filename(Path::new("/movies/Some Random Movie.mp4"));
        assert_eq!(p.title, "Some Random Movie");
        assert_eq!(p.year, None);
        assert_eq!(p.media_type, MediaType::Movie);
    }

    #[test]
    fn parse_tv_standard() {
        let p = parse_filename(Path::new("/tv/Breaking.Bad.S01E01.Pilot.720p.mkv"));
        assert_eq!(p.media_type, MediaType::Episode);
        assert_eq!(p.show_name, Some("Breaking Bad".to_string()));
        assert_eq!(p.season_number, Some(1));
        assert_eq!(p.episode_number, Some(1));
    }

    #[test]
    fn parse_tv_scene_naming() {
        let p = parse_filename(Path::new("/tv/The.Office.S02E15.Boys.and.Girls.1080p.WEB-DL.mkv"));
        assert_eq!(p.media_type, MediaType::Episode);
        assert_eq!(p.show_name, Some("The Office".to_string()));
        assert_eq!(p.season_number, Some(2));
        assert_eq!(p.episode_number, Some(15));
    }

    #[test]
    fn parse_tv_lowercase_sxxexx() {
        let p = parse_filename(Path::new("/tv/show.s03e22.mkv"));
        assert_eq!(p.media_type, MediaType::Episode);
        assert_eq!(p.season_number, Some(3));
        assert_eq!(p.episode_number, Some(22));
    }

    #[test]
    fn parse_tv_three_digit_episode() {
        let p = parse_filename(Path::new("/tv/Pokemon.S01E155.mkv"));
        assert_eq!(p.episode_number, Some(155));
    }

    #[test]
    fn parse_cleans_quality_suffixes() {
        let p = parse_filename(Path::new("/movies/Movie.Name.2020.2160p.UHD.BluRay.HEVC.mkv"));
        assert_eq!(p.title, "Movie Name");
        assert_eq!(p.year, Some(2020));
    }

    #[test]
    fn parse_underscores() {
        let p = parse_filename(Path::new("/movies/My_Movie_2015.mp4"));
        assert_eq!(p.title, "My Movie");
        assert_eq!(p.year, Some(2015));
    }

    #[test]
    fn parse_year_out_of_range() {
        let p = parse_filename(Path::new("/movies/Title.1800.mkv"));
        assert_eq!(p.year, None);
    }

    #[test]
    fn sort_title_strips_articles() {
        assert_eq!(make_sort_title("The Matrix"), "matrix");
        assert_eq!(make_sort_title("A Beautiful Mind"), "beautiful mind");
        assert_eq!(make_sort_title("An Officer"), "officer");
        assert_eq!(make_sort_title("Matrix"), "matrix");
    }

    #[test]
    fn sort_title_case_insensitive() {
        assert_eq!(make_sort_title("THE GODFATHER"), "godfather");
    }

    #[test]
    fn subtitle_language_english() {
        assert_eq!(
            extract_subtitle_language("movie.en", "movie"),
            Some("en".to_string())
        );
    }

    #[test]
    fn subtitle_language_three_letter() {
        assert_eq!(
            extract_subtitle_language("movie.eng", "movie"),
            Some("eng".to_string())
        );
    }

    #[test]
    fn subtitle_language_with_forced() {
        assert_eq!(
            extract_subtitle_language("movie.en.forced", "movie"),
            Some("en".to_string())
        );
    }

    #[test]
    fn subtitle_language_none() {
        assert_eq!(extract_subtitle_language("movie", "movie"), None);
    }

    #[test]
    fn subtitle_language_dash_separator() {
        assert_eq!(
            extract_subtitle_language("movie-eng", "movie"),
            Some("eng".to_string())
        );
    }

    #[test]
    fn macos_resource_fork_detected() {
        assert!(is_macos_resource_fork(Path::new("/foo/._bar.mkv")));
        assert!(!is_macos_resource_fork(Path::new("/foo/bar.mkv")));
        assert!(!is_macos_resource_fork(Path::new("/foo/.hidden.mkv")));
    }

    #[test]
    fn clean_title_strips_codec_info() {
        assert_eq!(clean_title_suffix("Movie Name 1080p BluRay x264"), "Movie Name");
        assert_eq!(clean_title_suffix("Show Title HEVC DTS-HD"), "Show Title");
        assert_eq!(clean_title_suffix("Clean Title"), "Clean Title");
    }
}
