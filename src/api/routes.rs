use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncSeekExt;
use tokio::sync::broadcast;
use tokio_util::io::ReaderStream;
use tracing::error;

use crate::api::AppState;
use crate::db::models::{
    EpisodeSummary, MediaItem, MediaType, MovieSummary, PlaybackState, Subtitle, TvShow,
    TvShowSummary,
};
use crate::ffmpeg::FFmpeg;
use crate::hls::HlsStatus;
use crate::metadata::TmdbClient;
use crate::scanner::Scanner;

type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        error!("Request error: {:?}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

// ── Library Routes ──────────────────────────────────────────────────────────

pub fn library_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/library/movies", get(list_movies))
        .route("/api/library/movies/{id}", get(get_movie))
        .route("/api/library/shows", get(list_shows))
        .route("/api/library/shows/{id}", get(get_show))
        .route(
            "/api/library/shows/{id}/seasons/{season}",
            get(get_season_episodes),
        )
        .route("/api/library/recent", get(recent_items))
        .route("/api/library/search", get(search_library))
}

#[derive(Deserialize)]
struct PaginationParams {
    page: Option<u32>,
    per_page: Option<u32>,
    sort: Option<String>,
}

#[derive(Serialize)]
struct PaginatedResponse<T: Serialize> {
    items: Vec<T>,
    total: i64,
    page: u32,
    per_page: u32,
}

async fn list_movies(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> AppResult<Json<PaginatedResponse<MovieSummary>>> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).min(200);
    let offset = (page - 1) * per_page;

    let order = match params.sort.as_deref() {
        Some("title") => "sort_title ASC",
        Some("year") => "year DESC, sort_title ASC",
        Some("added") => "added_at DESC",
        Some("rating") => "rating DESC NULLS LAST",
        _ => "sort_title ASC",
    };

    let conn = state.db.conn();

    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM media_items WHERE media_type = 'movie'",
        [],
        |row| row.get(0),
    )?;

    let query = format!(
        "SELECT id, title, year, poster_path, rating, duration_secs, video_width, video_height, hdr_format
         FROM media_items WHERE media_type = 'movie' ORDER BY {} LIMIT ?1 OFFSET ?2",
        order
    );

    let mut stmt = conn.prepare(&query)?;
    let movies: Vec<MovieSummary> = stmt
        .query_map(rusqlite::params![per_page, offset], |row| {
            Ok(MovieSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                year: row.get(2)?,
                poster_path: row.get(3)?,
                rating: row.get(4)?,
                duration_secs: row.get(5)?,
                video_width: row.get(6)?,
                video_height: row.get(7)?,
                hdr_format: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(PaginatedResponse {
        items: movies,
        total,
        page,
        per_page,
    }))
}

async fn get_movie(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let item: Option<MediaItem> = conn
        .query_row(
            "SELECT id, title, sort_title, media_type, year, file_path, file_size,
             duration_secs, video_codec, video_width, video_height, video_bitrate,
             hdr_format, audio_codec, audio_channels, audio_bitrate,
             show_name, season_number, episode_number, episode_title,
             tmdb_id, overview, poster_path, backdrop_path, genres, rating,
             release_date, added_at, updated_at
             FROM media_items WHERE id = ?1",
            [&id],
            |row| {
                Ok(MediaItem {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    sort_title: row.get(2)?,
                    media_type: row
                        .get::<_, String>(3)?
                        .parse()
                        .unwrap_or(MediaType::Movie),
                    year: row.get(4)?,
                    file_path: row.get(5)?,
                    file_size: row.get(6)?,
                    duration_secs: row.get(7)?,
                    video_codec: row.get(8)?,
                    video_width: row.get(9)?,
                    video_height: row.get(10)?,
                    video_bitrate: row.get(11)?,
                    hdr_format: row.get(12)?,
                    audio_codec: row.get(13)?,
                    audio_channels: row.get(14)?,
                    audio_bitrate: row.get(15)?,
                    show_name: row.get(16)?,
                    season_number: row.get(17)?,
                    episode_number: row.get(18)?,
                    episode_title: row.get(19)?,
                    tmdb_id: row.get(20)?,
                    overview: row.get(21)?,
                    poster_path: row.get(22)?,
                    backdrop_path: row.get(23)?,
                    genres: row.get(24)?,
                    rating: row.get(25)?,
                    release_date: row.get(26)?,
                    added_at: row.get(27)?,
                    updated_at: row.get(28)?,
                })
            },
        )
        .ok();

    match item {
        Some(movie) => {
            let subtitles = get_subtitles_for_media(&conn, &movie.id)?;
            let playback = get_playback_state(&conn, &movie.id)?;

            Ok(Json(serde_json::json!({
                "item": movie,
                "subtitles": subtitles,
                "playback": playback,
            }))
            .into_response())
        }
        None => Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
            .into_response()),
    }
}

async fn list_shows(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<Vec<TvShowSummary>>> {
    let conn = state.db.conn();

    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.poster_path, t.rating,
                (SELECT COUNT(DISTINCT m.season_number) FROM media_items m WHERE m.show_name = t.name AND m.media_type = 'episode'),
                (SELECT COUNT(*) FROM media_items m WHERE m.show_name = t.name AND m.media_type = 'episode')
         FROM tv_shows t ORDER BY t.name"
    )?;

    let shows: Vec<TvShowSummary> = stmt
        .query_map([], |row| {
            Ok(TvShowSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                poster_path: row.get(2)?,
                rating: row.get(3)?,
                season_count: row.get(4)?,
                episode_count: row.get(5)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(shows))
}

async fn get_show(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let show: Option<TvShow> = conn
        .query_row(
            "SELECT id, name, tmdb_id, overview, poster_path, backdrop_path, genres, rating, first_air_date, added_at
             FROM tv_shows WHERE id = ?1",
            [&id],
            |row| {
                Ok(TvShow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    tmdb_id: row.get(2)?,
                    overview: row.get(3)?,
                    poster_path: row.get(4)?,
                    backdrop_path: row.get(5)?,
                    genres: row.get(6)?,
                    rating: row.get(7)?,
                    first_air_date: row.get(8)?,
                    added_at: row.get(9)?,
                })
            },
        )
        .ok();

    match show {
        Some(show) => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT season_number FROM media_items
                 WHERE show_name = ?1 AND media_type = 'episode' AND season_number IS NOT NULL
                 ORDER BY season_number"
            )?;
            let seasons: Vec<i32> = stmt
                .query_map([&show.name], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(Json(serde_json::json!({
                "show": show,
                "seasons": seasons,
            }))
            .into_response())
        }
        None => Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
            .into_response()),
    }
}

async fn get_season_episodes(
    State(state): State<Arc<AppState>>,
    Path((id, season)): Path<(String, i32)>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let show_name: Option<String> = conn
        .query_row("SELECT name FROM tv_shows WHERE id = ?1", [&id], |row| {
            row.get(0)
        })
        .ok();

    let Some(show_name) = show_name else {
        return Ok(
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Show not found" })))
                .into_response(),
        );
    };

    let mut stmt = conn.prepare(
        "SELECT m.id, m.season_number, m.episode_number, m.episode_title, m.duration_secs,
                COALESCE(p.is_watched, 0), COALESCE(p.position_secs, 0)
         FROM media_items m
         LEFT JOIN playback_state p ON m.id = p.media_id
         WHERE m.show_name = ?1 AND m.media_type = 'episode' AND m.season_number = ?2
         ORDER BY m.episode_number",
    )?;

    let episodes: Vec<EpisodeSummary> = stmt
        .query_map(rusqlite::params![show_name, season], |row| {
            Ok(EpisodeSummary {
                id: row.get(0)?,
                season_number: row.get(1)?,
                episode_number: row.get(2)?,
                episode_title: row.get(3)?,
                duration_secs: row.get(4)?,
                is_watched: row.get::<_, i32>(5)? != 0,
                position_secs: row.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(episodes).into_response())
}

async fn recent_items(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> AppResult<Json<Vec<MovieSummary>>> {
    let limit = params.per_page.unwrap_or(20).min(100);
    let conn = state.db.conn();

    let mut stmt = conn.prepare(
        "SELECT id, title, year, poster_path, rating, duration_secs, video_width, video_height, hdr_format
         FROM media_items ORDER BY added_at DESC LIMIT ?1",
    )?;

    let items: Vec<MovieSummary> = stmt
        .query_map([limit], |row| {
            Ok(MovieSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                year: row.get(2)?,
                poster_path: row.get(3)?,
                rating: row.get(4)?,
                duration_secs: row.get(5)?,
                video_width: row.get(6)?,
                video_height: row.get(7)?,
                hdr_format: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(items))
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
}

async fn search_library(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> AppResult<Json<Vec<MovieSummary>>> {
    let conn = state.db.conn();
    let escaped = params.q.replace('%', "\\%").replace('_', "\\_");
    let query = format!("%{}%", escaped);

    let mut stmt = conn.prepare(
        "SELECT id, title, year, poster_path, rating, duration_secs, video_width, video_height, hdr_format
         FROM media_items WHERE title LIKE ?1 ESCAPE '\\' OR show_name LIKE ?1 ESCAPE '\\' OR episode_title LIKE ?1 ESCAPE '\\'
         ORDER BY sort_title LIMIT 50",
    )?;

    let items: Vec<MovieSummary> = stmt
        .query_map([&query], |row| {
            Ok(MovieSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                year: row.get(2)?,
                poster_path: row.get(3)?,
                rating: row.get(4)?,
                duration_secs: row.get(5)?,
                video_width: row.get(6)?,
                video_height: row.get(7)?,
                hdr_format: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(items))
}

// ── Playback Routes ─────────────────────────────────────────────────────────

pub fn playback_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/playback/{id}/state", get(get_playback).put(update_playback))
        .route("/api/playback/{id}/watched", post(mark_watched).delete(mark_unwatched))
}

async fn get_playback(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Json<PlaybackState>> {
    let conn = state.db.conn();
    let ps = get_playback_state(&conn, &id)?;
    Ok(Json(ps.unwrap_or(PlaybackState {
        media_id: id,
        position_secs: 0.0,
        is_watched: false,
        last_played_at: String::new(),
    })))
}

#[derive(Deserialize)]
struct UpdatePlaybackRequest {
    position_secs: f64,
}

async fn update_playback(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdatePlaybackRequest>,
) -> AppResult<StatusCode> {
    let conn = state.db.conn();
    conn.execute(
        "INSERT INTO playback_state (media_id, position_secs, last_played_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(media_id) DO UPDATE SET position_secs = ?2, last_played_at = datetime('now')",
        rusqlite::params![id, body.position_secs],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_watched(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let conn = state.db.conn();
    conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, last_played_at)
         VALUES (?1, 1, datetime('now'))
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 1, last_played_at = datetime('now')",
        [&id],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_unwatched(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let conn = state.db.conn();
    conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         VALUES (?1, 0, 0, datetime('now'))
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 0, position_secs = 0, last_played_at = datetime('now')",
        [&id],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Streaming Routes ────────────────────────────────────────────────────────

pub fn streaming_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/stream/{id}/info", get(stream_info))
        .route("/api/stream/{id}/hls/prepare", post(hls_prepare))
        .route("/api/stream/{id}/hls/status", get(hls_status))
        .route("/api/stream/{id}/hls/playlist.m3u8", get(hls_playlist))
        .route("/api/stream/{id}/hls/{segment}", get(hls_segment))
        .route("/api/stream/{id}/direct", get(direct_stream))
        .route("/api/stream/{id}/subtitle/{sub_id}", get(serve_subtitle))
}

#[derive(Serialize)]
struct StreamInfo {
    id: String,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    video_width: Option<i32>,
    video_height: Option<i32>,
    hdr_format: Option<String>,
    duration_secs: Option<f64>,
    file_size: i64,
    needs_transcode: bool,
    can_direct_play: bool,
    subtitles: Vec<Subtitle>,
}

struct StreamInfoRow {
    id: String,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    video_width: Option<i32>,
    video_height: Option<i32>,
    hdr_format: Option<String>,
    duration_secs: Option<f64>,
    file_size: i64,
    file_path: String,
}

async fn stream_info(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let item: Option<StreamInfoRow> = conn
        .query_row(
            "SELECT id, video_codec, audio_codec, video_width, video_height, hdr_format, duration_secs, file_size, file_path
             FROM media_items WHERE id = ?1",
            [&id],
            |row| {
                Ok(StreamInfoRow {
                    id: row.get(0)?,
                    video_codec: row.get(1)?,
                    audio_codec: row.get(2)?,
                    video_width: row.get(3)?,
                    video_height: row.get(4)?,
                    hdr_format: row.get(5)?,
                    duration_secs: row.get(6)?,
                    file_size: row.get(7)?,
                    file_path: row.get(8)?,
                })
            },
        )
        .ok();

    let Some(item) = item else {
        return Ok(
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
                .into_response(),
        );
    };

    let can_direct = item.video_codec.as_deref().map(FFmpeg::is_ios_native_video).unwrap_or(false)
        && !item.audio_codec.as_deref().map(FFmpeg::needs_audio_transcode).unwrap_or(true)
        && item.file_path.ends_with(".mp4");

    let needs_transcode = item
        .video_codec
        .as_deref()
        .map(|c| !FFmpeg::is_ios_native_video(c))
        .unwrap_or(true);

    let subtitles = get_subtitles_for_media(&conn, &id)?;

    Ok(Json(StreamInfo {
        id: item.id,
        video_codec: item.video_codec,
        audio_codec: item.audio_codec,
        video_width: item.video_width,
        video_height: item.video_height,
        hdr_format: item.hdr_format,
        duration_secs: item.duration_secs,
        file_size: item.file_size,
        needs_transcode,
        can_direct_play: can_direct,
        subtitles,
    })
    .into_response())
}

async fn hls_prepare(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let item: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT file_path, video_codec, audio_codec FROM media_items WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();

    let Some((file_path, video_codec, audio_codec)) = item else {
        return Ok(
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
                .into_response(),
        );
    };

    let hls = state.hls.clone();
    let media_id = id.clone();
    tokio::spawn(async move {
        if let Err(e) = hls
            .prepare_stream(
                &media_id,
                &file_path,
                video_codec.as_deref(),
                audio_codec.as_deref(),
            )
            .await
        {
            error!("HLS preparation failed for {}: {}", media_id, e);
        }
    });

    Ok(Json(serde_json::json!({ "status": "preparing" })).into_response())
}

async fn hls_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let status = state.hls.session_status(&id);
    let (status_str, error) = match status {
        Some(HlsStatus::Preparing) => ("preparing", None),
        Some(HlsStatus::Ready) => ("ready", None),
        Some(HlsStatus::Error(e)) => ("error", Some(e)),
        None => ("not_found", None),
    };

    Ok(Json(serde_json::json!({
        "status": status_str,
        "error": error,
    })))
}

async fn hls_playlist(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let Some(path) = state.hls.playlist_path(&id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    let content = tokio::fs::read_to_string(&path).await?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        content,
    )
        .into_response())
}

async fn hls_segment(
    State(state): State<Arc<AppState>>,
    Path((id, segment)): Path<(String, String)>,
) -> AppResult<Response> {
    if segment.contains("..") || segment.contains('/') || segment.contains('\\') {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let Some(path) = state.hls.segment_path(&id, &segment) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    let file = tokio::fs::File::open(&path).await?;
    let metadata = file.metadata().await?;
    let stream = ReaderStream::new(file);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp2t")
        .header(header::CONTENT_LENGTH, metadata.len())
        .body(Body::from_stream(stream))
        .unwrap())
}

async fn direct_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let item: Option<(String, i64)> = {
        let conn = state.db.conn();
        conn.query_row(
            "SELECT file_path, file_size FROM media_items WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
    };

    let Some((file_path, file_size)) = item else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    let file_size = file_size as u64;
    let content_type = mime_guess::from_path(&file_path)
        .first_or_octet_stream()
        .to_string();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());

    if let Some(range_str) = range_header {
        let (start, end) = parse_range(range_str, file_size)?;
        let content_length = end - start + 1;

        let mut file = tokio::fs::File::open(&file_path).await?;
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let reader = tokio::io::AsyncReadExt::take(file, content_length);
        let stream = ReaderStream::new(reader);

        Ok(Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, &content_type)
            .header(header::CONTENT_LENGTH, content_length)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", start, end, file_size),
            )
            .body(Body::from_stream(stream))
            .unwrap())
    } else {
        let file = tokio::fs::File::open(&file_path).await?;
        let stream = ReaderStream::new(file);

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &content_type)
            .header(header::CONTENT_LENGTH, file_size)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::from_stream(stream))
            .unwrap())
    }
}

fn parse_range(range: &str, file_size: u64) -> Result<(u64, u64), AppError> {
    let range = range
        .strip_prefix("bytes=")
        .ok_or_else(|| anyhow::anyhow!("Invalid range header"))?;

    let parts: Vec<&str> = range.split('-').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!("Invalid range format").into());
    }

    let (start, end) = if parts[0].is_empty() {
        let suffix: u64 = parts[1].parse().map_err(|_| anyhow::anyhow!("Invalid range"))?;
        (file_size.saturating_sub(suffix), file_size - 1)
    } else {
        let s: u64 = parts[0].parse().map_err(|_| anyhow::anyhow!("Invalid range"))?;
        let e: u64 = if parts[1].is_empty() {
            file_size - 1
        } else {
            parts[1].parse().map_err(|_| anyhow::anyhow!("Invalid range"))?
        };
        (s, e)
    };

    let end = end.min(file_size - 1);

    if start > end || start >= file_size {
        return Err(anyhow::anyhow!("Range not satisfiable").into());
    }

    Ok((start, end))
}

async fn serve_subtitle(
    State(state): State<Arc<AppState>>,
    Path((id, sub_id)): Path<(String, String)>,
) -> AppResult<Response> {
    let sub: Option<Subtitle> = {
        let conn = state.db.conn();
        conn.query_row(
            "SELECT id, media_id, file_path, stream_index, language, codec, is_forced, is_default, is_external
             FROM subtitles WHERE id = ?1 AND media_id = ?2",
            rusqlite::params![sub_id, id],
            |row| {
                Ok(Subtitle {
                    id: row.get(0)?,
                    media_id: row.get(1)?,
                    file_path: row.get(2)?,
                    stream_index: row.get(3)?,
                    language: row.get(4)?,
                    codec: row.get(5)?,
                    is_forced: row.get::<_, i32>(6)? != 0,
                    is_default: row.get::<_, i32>(7)? != 0,
                    is_external: row.get::<_, i32>(8)? != 0,
                })
            },
        )
        .ok()
    };

    let Some(sub) = sub else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    if sub.is_external
        && let Some(ref path) = sub.file_path
    {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if ext == "vtt" {
            let content = tokio::fs::read_to_string(path).await?;
            return Ok(([(header::CONTENT_TYPE, "text/vtt")], content).into_response());
        }

        if ext == "srt" {
            let content = tokio::fs::read_to_string(path).await?;
            let vtt = srt_to_vtt(&content);
            return Ok(([(header::CONTENT_TYPE, "text/vtt")], vtt).into_response());
        }

        let content = tokio::fs::read_to_string(path).await?;
        return Ok(([(header::CONTENT_TYPE, "text/plain")], content).into_response());
    }

    if let Some(stream_index) = sub.stream_index {
        let codec = sub.codec.as_deref().unwrap_or("");
        if matches!(codec, "dvd_subtitle" | "hdmv_pgs_subtitle" | "pgssub" | "vobsub" | "dvb_subtitle") {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": "Bitmap-based subtitles cannot be converted to text" })),
            ).into_response());
        }

        let media_path: String = {
            let conn = state.db.conn();
            conn.query_row(
                "SELECT file_path FROM media_items WHERE id = ?1",
                [&id],
                |row| row.get(0),
            )?
        };

        let vtt_path = state
            .config
            .transcoding
            .cache_dir
            .join("subs")
            .join(format!("{}_{}.vtt", id, sub_id));

        if !vtt_path.exists() {
            state
                .ffmpeg
                .extract_subtitle_vtt(
                    std::path::Path::new(&media_path),
                    stream_index,
                    &vtt_path,
                )
                .await?;
        }

        let content = tokio::fs::read_to_string(&vtt_path).await?;
        return Ok(([(header::CONTENT_TYPE, "text/vtt")], content).into_response());
    }

    Ok(StatusCode::NOT_FOUND.into_response())
}

fn srt_to_vtt(srt: &str) -> String {
    let mut vtt = String::from("WEBVTT\n\n");
    for line in srt.lines() {
        if line.contains(" --> ") {
            vtt.push_str(&line.replace(',', "."));
        } else {
            vtt.push_str(line);
        }
        vtt.push('\n');
    }
    vtt
}

// ── Metadata Routes ─────────────────────────────────────────────────────────

pub fn metadata_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/metadata/scan", post(trigger_scan))
        .route("/api/metadata/refresh", post(trigger_refresh))
        .route("/api/metadata/image/{*path}", get(proxy_image))
}

async fn trigger_scan(State(state): State<Arc<AppState>>) -> AppResult<Json<serde_json::Value>> {
    let db = state.db.clone();
    let ffmpeg = state.ffmpeg.clone();
    let dirs = state.config.library.media_dirs.clone();

    tokio::spawn(async move {
        let scanner = Scanner::new(db, ffmpeg);
        if let Err(e) = scanner.scan_directories(&dirs).await {
            error!("Library scan failed: {}", e);
        }
    });

    Ok(Json(serde_json::json!({ "status": "scan_started" })))
}

async fn trigger_refresh(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<serde_json::Value>> {
    let tmdb = state.tmdb.clone();
    let db = state.db.clone();

    tokio::spawn(async move {
        if let Err(e) = tmdb.migrate_numeric_genres(&db).await {
            error!("Genre migration failed: {}", e);
        }
        if let Err(e) = tmdb.update_movie_metadata(&db).await {
            error!("Movie metadata refresh failed: {}", e);
        }
        if let Err(e) = tmdb.update_tv_metadata(&db).await {
            error!("TV metadata refresh failed: {}", e);
        }
    });

    Ok(Json(serde_json::json!({ "status": "refresh_started" })))
}

const VALID_IMAGE_SIZES: &[&str] = &[
    "w92", "w154", "w185", "w342", "w500", "w780", "original",
];

#[derive(Deserialize)]
struct ImageQuery {
    #[serde(default = "default_image_size")]
    size: String,
}

fn default_image_size() -> String {
    "w500".to_string()
}

async fn proxy_image(
    State(state): State<Arc<AppState>>,
    Path(tmdb_path): Path<String>,
    Query(query): Query<ImageQuery>,
) -> AppResult<Response> {
    if !VALID_IMAGE_SIZES.contains(&query.size.as_str()) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid size", "valid": VALID_IMAGE_SIZES })),
        )
            .into_response());
    }

    let cache_dir = state.config.transcoding.cache_dir.join("images");
    let size_dir = cache_dir.join(&query.size);

    let safe_name = tmdb_path.replace('/', "_");
    let cache_path = size_dir.join(&safe_name);
    let content_type = content_type_for_image(&tmdb_path);

    if cache_path.exists() {
        return serve_cached_image(&cache_path, content_type).await;
    }

    let cache_key = format!("{}/{}", query.size, safe_name);

    let is_leader;
    let rx = {
        let entry = state.image_fetches.entry(cache_key.clone());
        match entry {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                is_leader = false;
                Some(e.get().subscribe())
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                is_leader = true;
                let (tx, _) = broadcast::channel(1);
                e.insert(tx);
                None
            }
        }
    };

    if !is_leader {
        if let Some(mut rx) = rx {
            let _ = rx.recv().await;
        }
        if cache_path.exists() {
            return serve_cached_image(&cache_path, content_type).await;
        }
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let result = fetch_and_cache_image(&tmdb_path, &query.size, &size_dir, &cache_path).await;

    if let Some((_, tx)) = state.image_fetches.remove(&cache_key) {
        let _ = tx.send(());
    }

    match result {
        Ok(data) => Ok(image_response(content_type, Body::from(data))),
        Err(_) => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

async fn serve_cached_image(
    cache_path: &std::path::Path,
    content_type: &str,
) -> AppResult<Response> {
    let file = tokio::fs::File::open(cache_path).await?;
    let stream = ReaderStream::new(file);
    Ok(image_response(content_type, Body::from_stream(stream)))
}

fn image_response(content_type: &str, body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=604800, immutable")
        .body(body)
        .unwrap()
}

async fn fetch_and_cache_image(
    tmdb_path: &str,
    size: &str,
    size_dir: &std::path::Path,
    cache_path: &std::path::Path,
) -> Result<Vec<u8>, anyhow::Error> {
    let url = TmdbClient::poster_url(&format!("/{}", tmdb_path), size);
    let resp = reqwest::get(&url).await?;

    if !resp.status().is_success() {
        anyhow::bail!("TMDB returned {}", resp.status());
    }

    let data = resp.bytes().await?;
    tokio::fs::create_dir_all(size_dir).await?;
    tokio::fs::write(cache_path, &data).await?;

    Ok(data.to_vec())
}

fn content_type_for_image(path: &str) -> &'static str {
    if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "image/jpeg"
    }
}

// ── System Routes ───────────────────────────────────────────────────────────

pub fn system_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/system/health", get(health))
        .route("/api/system/stats", get(stats))
        .route("/api/system/config", get(get_config))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn stats(State(state): State<Arc<AppState>>) -> AppResult<Json<serde_json::Value>> {
    let conn = state.db.conn();

    let movie_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM media_items WHERE media_type = 'movie'",
        [],
        |row| row.get(0),
    )?;

    let episode_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM media_items WHERE media_type = 'episode'",
        [],
        |row| row.get(0),
    )?;

    let show_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM tv_shows", [], |row| row.get(0))?;

    let total_size: i64 = conn.query_row(
        "SELECT COALESCE(SUM(file_size), 0) FROM media_items",
        [],
        |row| row.get(0),
    )?;

    let total_duration: f64 = conn.query_row(
        "SELECT COALESCE(SUM(duration_secs), 0) FROM media_items",
        [],
        |row| row.get(0),
    )?;

    Ok(Json(serde_json::json!({
        "movies": movie_count,
        "episodes": episode_count,
        "shows": show_count,
        "total_size_bytes": total_size,
        "total_duration_secs": total_duration,
    })))
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "server": {
            "host": state.config.server.host,
            "port": state.config.server.port,
        },
        "library": {
            "media_dirs": state.config.library.media_dirs,
            "scan_interval_secs": state.config.library.scan_interval_secs,
        },
        "transcoding": {
            "hls_segment_duration": state.config.transcoding.hls_segment_duration,
            "max_concurrent_transcodes": state.config.transcoding.max_concurrent_transcodes,
            "cache_dir": state.config.transcoding.cache_dir,
        },
        "tmdb": {
            "has_api_key": !state.config.tmdb.api_key.is_empty(),
            "language": state.config.tmdb.language,
        },
    }))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn get_subtitles_for_media(
    conn: &rusqlite::Connection,
    media_id: &str,
) -> Result<Vec<Subtitle>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, file_path, stream_index, language, codec, is_forced, is_default, is_external
         FROM subtitles WHERE media_id = ?1",
    )?;

    let subs = stmt
        .query_map([media_id], |row| {
            Ok(Subtitle {
                id: row.get(0)?,
                media_id: row.get(1)?,
                file_path: row.get(2)?,
                stream_index: row.get(3)?,
                language: row.get(4)?,
                codec: row.get(5)?,
                is_forced: row.get::<_, i32>(6)? != 0,
                is_default: row.get::<_, i32>(7)? != 0,
                is_external: row.get::<_, i32>(8)? != 0,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(subs)
}

fn get_playback_state(
    conn: &rusqlite::Connection,
    media_id: &str,
) -> Result<Option<PlaybackState>, rusqlite::Error> {
    conn.query_row(
        "SELECT media_id, position_secs, is_watched, last_played_at FROM playback_state WHERE media_id = ?1",
        [media_id],
        |row| {
            Ok(PlaybackState {
                media_id: row.get(0)?,
                position_secs: row.get(1)?,
                is_watched: row.get::<_, i32>(2)? != 0,
                last_played_at: row.get(3)?,
            })
        },
    )
    .ok()
    .map(Ok)
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_standard() {
        let (start, end) = parse_range("bytes=0-999", 10000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 999);
    }

    #[test]
    fn range_open_ended() {
        let (start, end) = parse_range("bytes=5000-", 10000).unwrap();
        assert_eq!(start, 5000);
        assert_eq!(end, 9999);
    }

    #[test]
    fn range_suffix() {
        let (start, end) = parse_range("bytes=-500", 10000).unwrap();
        assert_eq!(start, 9500);
        assert_eq!(end, 9999);
    }

    #[test]
    fn range_suffix_larger_than_file() {
        let (start, end) = parse_range("bytes=-50000", 10000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 9999);
    }

    #[test]
    fn range_end_clamped_to_file_size() {
        let (start, end) = parse_range("bytes=0-99999", 10000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 9999);
    }

    #[test]
    fn range_single_byte() {
        let (start, end) = parse_range("bytes=0-0", 10000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }

    #[test]
    fn range_last_byte() {
        let (start, end) = parse_range("bytes=9999-9999", 10000).unwrap();
        assert_eq!(start, 9999);
        assert_eq!(end, 9999);
    }

    #[test]
    fn range_past_eof_fails() {
        assert!(parse_range("bytes=10000-10000", 10000).is_err());
    }

    #[test]
    fn range_start_greater_than_end_fails() {
        assert!(parse_range("bytes=500-100", 10000).is_err());
    }

    #[test]
    fn range_missing_prefix_fails() {
        assert!(parse_range("0-999", 10000).is_err());
    }

    #[test]
    fn range_garbage_fails() {
        assert!(parse_range("bytes=abc-def", 10000).is_err());
    }

    #[test]
    fn srt_to_vtt_basic() {
        let srt = "1\n00:00:01,000 --> 00:00:02,500\nHello world\n";
        let vtt = srt_to_vtt(srt);
        assert!(vtt.starts_with("WEBVTT"));
        assert!(vtt.contains("00:00:01.000 --> 00:00:02.500"));
        assert!(vtt.contains("Hello world"));
    }

    #[test]
    fn srt_to_vtt_preserves_text() {
        let srt = "1\n00:00:00,000 --> 00:00:01,000\nLine one\nLine two\n";
        let vtt = srt_to_vtt(srt);
        assert!(vtt.contains("Line one"));
        assert!(vtt.contains("Line two"));
    }

    #[test]
    fn srt_to_vtt_replaces_all_commas_in_timestamps() {
        let srt = "1\n00:01:23,456 --> 00:04:56,789\nText\n";
        let vtt = srt_to_vtt(srt);
        assert!(vtt.contains("00:01:23.456 --> 00:04:56.789"));
    }

    #[test]
    fn srt_to_vtt_preserves_commas_in_dialogue() {
        let srt = "1\n00:00:00,000 --> 00:00:01,000\nHello, world\n";
        let vtt = srt_to_vtt(srt);
        assert!(vtt.contains("Hello, world"));
        assert!(vtt.contains("00:00:00.000 --> 00:00:01.000"));
    }

    #[test]
    fn srt_to_vtt_empty_input() {
        let vtt = srt_to_vtt("");
        assert!(vtt.starts_with("WEBVTT"));
    }
}
