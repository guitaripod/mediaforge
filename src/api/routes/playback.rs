use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use crate::api::error::AppResult;
use crate::api::helpers::get_playback_state;
use crate::api::AppState;
use crate::db::models::PlaybackState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/playback/{id}/state", get(get_playback).put(update_playback))
        .route("/api/playback/{id}/watched", axum::routing::post(mark_watched).delete(mark_unwatched))
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
) -> AppResult<Response> {
    if !body.position_secs.is_finite() || body.position_secs < 0.0 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "position_secs must be a non-negative finite number" })),
        ).into_response());
    }

    let conn = state.db.conn();
    conn.execute(
        "INSERT INTO playback_state (media_id, position_secs, last_played_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(media_id) DO UPDATE SET position_secs = ?2, last_played_at = datetime('now')",
        rusqlite::params![id, body.position_secs],
    )?;
    Ok(StatusCode::NO_CONTENT.into_response())
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
