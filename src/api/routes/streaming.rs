use std::sync::{Arc, LazyLock};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncSeekExt;
use tokio_util::io::ReaderStream;
use tracing::error;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::api::error::AppResult;
use crate::api::helpers::{get_audio_tracks_for_media, get_subtitles_for_media};
use crate::api::AppState;
use crate::db::models::{AudioTrack, Subtitle};
use crate::ffmpeg::FFmpeg;
use crate::hls::{HlsStatus, PrepareStreamParams};

static HLS_SEGMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^segment_\d{4}\.ts$").unwrap());

static HLS_VARIANT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(720p|360p|original)$").unwrap());

pub fn routes() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::new()
        .routes(routes!(stream_info))
        .routes(routes!(hls_prepare))
        .routes(routes!(hls_cancel))
        .routes(routes!(hls_status))
        .routes(routes!(hls_master))
        .routes(routes!(hls_variant_playlist))
        .routes(routes!(hls_segment))
        .routes(routes!(direct_stream))
        .routes(routes!(serve_sprite_vtt))
        .routes(routes!(serve_sprite_image))
        .routes(routes!(serve_subtitle))
}

#[derive(Serialize, ToSchema)]
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
    audio_tracks: Vec<AudioTrack>,
    sprites_vtt: String,
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

#[derive(Serialize, ToSchema)]
struct HlsStatusResponse {
    status: String,
    progress: Option<f32>,
    error: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct StatusMessage {
    status: String,
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/info",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, body = StreamInfo),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn stream_info(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let item = match conn.query_row(
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
    ) {
        Ok(item) => item,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Ok(
                (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
                    .into_response(),
            );
        }
        Err(e) => return Err(e.into()),
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
    let audio_tracks = get_audio_tracks_for_media(&conn, &id)?;

    let sprites_vtt = format!("/api/stream/{}/sprites/sprites.vtt", item.id);
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
        audio_tracks,
        sprites_vtt,
    })
    .into_response())
}

#[derive(Deserialize, Default, ToSchema)]
struct HlsPrepareRequest {
    audio_track_id: Option<String>,
    start_secs: Option<f64>,
}

#[utoipa::path(
    post,
    path = "/api/stream/{id}/hls/prepare",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    request_body = HlsPrepareRequest,
    responses(
        (status = 200, body = StatusMessage),
        (status = 400, body = crate::api::error::ErrorResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn hls_prepare(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<HlsPrepareRequest>>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    struct PrepareRow { file_path: String, video_codec: Option<String>, audio_codec: Option<String>, video_height: Option<i32>, duration_secs: Option<f64> }
    let PrepareRow { file_path, video_codec, mut audio_codec, video_height, duration_secs } = match conn
        .query_row(
            "SELECT file_path, video_codec, audio_codec, video_height, duration_secs FROM media_items WHERE id = ?1",
            [&id],
            |row| Ok(PrepareRow { file_path: row.get(0)?, video_codec: row.get(1)?, audio_codec: row.get(2)?, video_height: row.get(3)?, duration_secs: row.get(4)? }),
        ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Ok(
                (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
                    .into_response(),
            );
        }
        Err(e) => return Err(e.into()),
    };

    let req = body.map(|b| b.0).unwrap_or_default();
    let audio_stream_index = if let Some(ref track_id) = req.audio_track_id {
        match conn.query_row(
            "SELECT stream_index, codec FROM audio_tracks WHERE id = ?1 AND media_id = ?2",
            rusqlite::params![track_id, id],
            |row| Ok((row.get::<_, i32>(0)?, row.get::<_, String>(1)?)),
        ) {
            Ok((idx, codec)) => {
                audio_codec = Some(codec);
                Some(idx)
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Ok((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "Invalid audio track" })),
                )
                    .into_response());
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };

    let start_secs = req.start_secs;
    let hls = state.hls.clone();
    let media_id = id.clone();
    tokio::spawn(async move {
        if let Err(e) = hls
            .prepare_stream(PrepareStreamParams {
                media_id: &media_id,
                file_path: &file_path,
                video_codec: video_codec.as_deref(),
                audio_codec: audio_codec.as_deref(),
                audio_stream_index,
                source_height: video_height,
                duration_secs,
                start_secs,
            })
            .await
            && !e.to_string().contains("cancelled")
        {
            error!("HLS preparation failed for {}: {}", media_id, e);
        }
    });

    Ok(Json(serde_json::json!({ "status": "preparing" })).into_response())
}

#[utoipa::path(
    post,
    path = "/api/stream/{id}/hls/cancel",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, body = StatusMessage),
    )
)]
async fn hls_cancel(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    state.hls.cancel_media(&id);
    Ok(Json(serde_json::json!({ "status": "cancelled" })))
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/hls/status",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, body = HlsStatusResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn hls_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let status = state.hls.session_status(&id);
    let (status_str, progress, error) = match status {
        Some(HlsStatus::Preparing(pct)) => ("preparing", Some(pct), None),
        Some(HlsStatus::Ready) => ("ready", None, None),
        Some(HlsStatus::Error(e)) => ("error", None, Some(e)),
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "No active session" })),
            )
                .into_response());
        }
    };

    Ok(Json(serde_json::json!({
        "status": status_str,
        "progress": progress,
        "error": error,
    }))
    .into_response())
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/hls/master.m3u8",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, content_type = "application/vnd.apple.mpegurl", body = String),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn hls_master(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let Some(path) = state.hls.master_playlist_path(&id) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    };

    let content = tokio::fs::read_to_string(&path).await?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        content,
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/hls/{variant}/playlist.m3u8",
    tag = "streaming",
    params(
        ("id" = String, Path, description = "Media item ID"),
        ("variant" = String, Path, description = "HLS variant (720p, 360p, original)"),
    ),
    responses(
        (status = 200, content_type = "application/vnd.apple.mpegurl", body = String),
        (status = 400, body = crate::api::error::ErrorResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn hls_variant_playlist(
    State(state): State<Arc<AppState>>,
    Path((id, variant)): Path<(String, String)>,
) -> AppResult<Response> {
    if !HLS_VARIANT_RE.is_match(&variant) {
        return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "Invalid variant" }))).into_response());
    }

    let Some(path) = state.hls.variant_playlist_path(&id, &variant) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    };

    let content = tokio::fs::read_to_string(&path).await?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        content,
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/hls/{variant}/{segment}",
    tag = "streaming",
    params(
        ("id" = String, Path, description = "Media item ID"),
        ("variant" = String, Path, description = "HLS variant (720p, 360p, original)"),
        ("segment" = String, Path, description = "Segment filename (e.g. segment_0000.ts)"),
    ),
    responses(
        (status = 200, content_type = "video/mp2t"),
        (status = 400, body = crate::api::error::ErrorResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn hls_segment(
    State(state): State<Arc<AppState>>,
    Path((id, variant, segment)): Path<(String, String, String)>,
) -> AppResult<Response> {
    if !HLS_VARIANT_RE.is_match(&variant) || !HLS_SEGMENT_RE.is_match(&segment) {
        return Ok((StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "Invalid variant or segment name" }))).into_response());
    }

    let Some(path) = state.hls.segment_path(&id, &variant, &segment) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
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

#[utoipa::path(
    get,
    path = "/api/stream/{id}/direct",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, content_type = "application/octet-stream"),
        (status = 206, content_type = "application/octet-stream"),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 416, body = crate::api::error::ErrorResponse),
    )
)]
async fn direct_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let (file_path, file_size): (String, i64) = {
        let conn = state.db.conn();
        match conn.query_row(
            "SELECT file_path, file_size FROM media_items WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ) {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
            }
            Err(e) => return Err(e.into()),
        }
    };

    let file_size = file_size as u64;
    let content_type = mime_guess::from_path(&file_path)
        .first_or_octet_stream()
        .to_string();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());

    if let Some(range_str) = range_header {
        let (start, end) = match parse_range(range_str, file_size) {
            Ok(r) => r,
            Err(_) => return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                Json(serde_json::json!({ "error": "Range not satisfiable", "file_size": file_size })),
            ).into_response()),
        };
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

fn parse_range(range: &str, file_size: u64) -> Result<(u64, u64), StatusCode> {
    let range = range
        .strip_prefix("bytes=")
        .ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?;

    let parts: Vec<&str> = range.split('-').collect();
    if parts.len() != 2 {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }

    let (start, end) = if parts[0].is_empty() {
        let suffix: u64 = parts[1]
            .parse()
            .map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
        (file_size.saturating_sub(suffix), file_size - 1)
    } else {
        let s: u64 = parts[0]
            .parse()
            .map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
        let e: u64 = if parts[1].is_empty() {
            file_size - 1
        } else {
            parts[1]
                .parse()
                .map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?
        };
        (s, e)
    };

    let end = end.min(file_size - 1);

    if start > end || start >= file_size {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }

    Ok((start, end))
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/subtitle/{sub_id}",
    tag = "streaming",
    params(
        ("id" = String, Path, description = "Media item ID"),
        ("sub_id" = String, Path, description = "Subtitle ID"),
    ),
    responses(
        (status = 200, content_type = "text/vtt", body = String),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 422, body = crate::api::error::ErrorResponse),
    )
)]
async fn serve_subtitle(
    State(state): State<Arc<AppState>>,
    Path((id, sub_id)): Path<(String, String)>,
) -> AppResult<Response> {
    let sub = {
        let conn = state.db.conn();
        match conn.query_row(
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
        ) {
            Ok(sub) => sub,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
            }
            Err(e) => return Err(e.into()),
        }
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

        let subs_dir = state.config.transcoding.cache_dir.join("subs");
        let vtt_path = subs_dir.join(format!("{}_{}.vtt", id, sub_id));

        if !vtt_path.exists() {
            let tmp_path = subs_dir.join(format!("{}_{}.vtt.tmp", id, sub_id));
            match state
                .ffmpeg
                .extract_subtitle_vtt(
                    std::path::Path::new(&media_path),
                    stream_index,
                    &tmp_path,
                )
                .await
            {
                Ok(()) => {
                    tokio::fs::rename(&tmp_path, &vtt_path).await?;
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp_path).await;
                    return Err(e.into());
                }
            }
        }

        let content = tokio::fs::read_to_string(&vtt_path).await?;
        return Ok(([(header::CONTENT_TYPE, "text/vtt")], content).into_response());
    }

    Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response())
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/sprites/sprites.vtt",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, content_type = "text/vtt", body = String),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn serve_sprite_vtt(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let sprite_dir = state.config.transcoding.cache_dir.join("sprites").join(&id);
    let vtt_path = sprite_dir.join("sprites.vtt");

    if !vtt_path.exists() {
        generate_sprites_for_media(&state, &id, &sprite_dir).await?;
    }

    if !vtt_path.exists() {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    }

    let content = tokio::fs::read_to_string(&vtt_path).await?;
    Ok(([(header::CONTENT_TYPE, "text/vtt")], content).into_response())
}

#[utoipa::path(
    get,
    path = "/api/stream/{id}/sprites/sprites.jpg",
    tag = "streaming",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, content_type = "image/jpeg"),
        (status = 404, body = crate::api::error::ErrorResponse),
    )
)]
async fn serve_sprite_image(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let sprite_dir = state.config.transcoding.cache_dir.join("sprites").join(&id);
    let img_path = sprite_dir.join("sprites.jpg");

    if !img_path.exists() {
        generate_sprites_for_media(&state, &id, &sprite_dir).await?;
    }

    if !img_path.exists() {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    }

    let data = tokio::fs::read(&img_path).await?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], data).into_response())
}

async fn generate_sprites_for_media(
    state: &AppState,
    media_id: &str,
    sprite_dir: &std::path::Path,
) -> Result<(), crate::api::error::AppError> {
    let (file_path, duration): (String, Option<f64>) = {
        let conn = state.db.conn();
        conn.query_row(
            "SELECT file_path, duration_secs FROM media_items WHERE id = ?1",
            [media_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
    };

    let Some(duration) = duration else {
        return Ok(());
    };

    state
        .ffmpeg
        .generate_sprites(std::path::Path::new(&file_path), sprite_dir, duration)
        .await?;

    Ok(())
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
        assert_eq!(
            parse_range("bytes=10000-10000", 10000).unwrap_err(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }

    #[test]
    fn range_start_greater_than_end_fails() {
        assert_eq!(
            parse_range("bytes=500-100", 10000).unwrap_err(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }

    #[test]
    fn range_missing_prefix_fails() {
        assert_eq!(
            parse_range("0-999", 10000).unwrap_err(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }

    #[test]
    fn range_garbage_fails() {
        assert_eq!(
            parse_range("bytes=abc-def", 10000).unwrap_err(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
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

    #[test]
    fn segment_valid_names() {
        assert!(HLS_SEGMENT_RE.is_match("segment_0000.ts"));
        assert!(HLS_SEGMENT_RE.is_match("segment_0042.ts"));
        assert!(HLS_SEGMENT_RE.is_match("segment_9999.ts"));
    }

    #[test]
    fn segment_rejects_traversal() {
        assert!(!HLS_SEGMENT_RE.is_match("../etc/passwd"));
        assert!(!HLS_SEGMENT_RE.is_match("segment_0001.ts/../../etc/passwd"));
        assert!(!HLS_SEGMENT_RE.is_match("..\\segment_0001.ts"));
    }

    #[test]
    fn segment_rejects_wrong_format() {
        assert!(!HLS_SEGMENT_RE.is_match("playlist.m3u8"));
        assert!(!HLS_SEGMENT_RE.is_match("segment_01.ts"));
        assert!(!HLS_SEGMENT_RE.is_match("segment_00001.ts"));
        assert!(!HLS_SEGMENT_RE.is_match("other_0000.ts"));
        assert!(!HLS_SEGMENT_RE.is_match("segment_0000.mp4"));
    }

    #[test]
    fn variant_valid_names() {
        assert!(HLS_VARIANT_RE.is_match("720p"));
        assert!(HLS_VARIANT_RE.is_match("360p"));
        assert!(HLS_VARIANT_RE.is_match("original"));
    }

    #[test]
    fn variant_rejects_invalid() {
        assert!(!HLS_VARIANT_RE.is_match("1080p"));
        assert!(!HLS_VARIANT_RE.is_match("../etc"));
        assert!(!HLS_VARIANT_RE.is_match(""));
        assert!(!HLS_VARIANT_RE.is_match("720p/../../etc"));
    }
}
