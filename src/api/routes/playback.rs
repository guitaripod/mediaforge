use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::api::error::AppResult;
use crate::api::helpers::get_playback_state;
use crate::api::AppState;
use crate::db::models::{ActivityLogEntry, PlaybackState};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/playback/{id}/state", get(get_playback).put(update_playback))
        .route("/api/playback/{id}/watched", axum::routing::post(mark_watched).delete(mark_unwatched))
        .route("/api/playback/history", get(get_activity_history))
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
    event: Option<String>,
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

    let event_type = match body.event.as_deref() {
        Some("play") => "play",
        Some("pause") => "pause",
        None => "position_update",
        Some(other) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("invalid event type: {}", other) })),
            ).into_response());
        }
    };

    if event_type == "position_update" {
        conn.execute(
            "INSERT INTO activity_log (media_id, event_type, position_secs)
             SELECT ?1, ?2, ?3
             WHERE NOT EXISTS (
                 SELECT 1 FROM activity_log
                 WHERE media_id = ?1 AND event_type = 'position_update'
                   AND created_at > datetime('now', '-30 seconds')
             )",
            rusqlite::params![id, event_type, body.position_secs],
        )?;
    } else {
        conn.execute(
            "INSERT INTO activity_log (media_id, event_type, position_secs) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, event_type, body.position_secs],
        )?;
    }

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
    conn.execute(
        "INSERT INTO activity_log (media_id, event_type, position_secs) VALUES (?1, 'complete', 0)",
        [&id],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct HistoryParams {
    media_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

#[derive(Serialize)]
struct HistoryResponse {
    entries: Vec<ActivityLogEntry>,
    total: i64,
    limit: u32,
    offset: u32,
}

async fn get_activity_history(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryParams>,
) -> AppResult<Json<HistoryResponse>> {
    let limit = params.limit.unwrap_or(50).min(200);
    let offset = params.offset.unwrap_or(0);
    let conn = state.db.conn();

    fn map_activity_row(row: &rusqlite::Row) -> rusqlite::Result<ActivityLogEntry> {
        Ok(ActivityLogEntry {
            id: row.get(0)?,
            media_id: row.get(1)?,
            event_type: row.get(2)?,
            position_secs: row.get(3)?,
            created_at: row.get(4)?,
            title: row.get(5)?,
            media_type: row.get(6)?,
        })
    }

    let (total, entries) = if let Some(ref media_id) = params.media_id {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM activity_log WHERE media_id = ?1",
            [media_id],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT al.id, al.media_id, al.event_type, al.position_secs, al.created_at, m.title, m.media_type
             FROM activity_log al
             LEFT JOIN media_items m ON al.media_id = m.id
             WHERE al.media_id = ?1
             ORDER BY al.created_at DESC LIMIT ?2 OFFSET ?3",
        )?;
        let entries: Vec<ActivityLogEntry> = stmt
            .query_map(rusqlite::params![media_id, limit, offset], map_activity_row)?
            .filter_map(|r| r.ok())
            .collect();
        (total, entries)
    } else {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM activity_log",
            [],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT al.id, al.media_id, al.event_type, al.position_secs, al.created_at, m.title, m.media_type
             FROM activity_log al
             LEFT JOIN media_items m ON al.media_id = m.id
             ORDER BY al.created_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let entries: Vec<ActivityLogEntry> = stmt
            .query_map(rusqlite::params![limit, offset], map_activity_row)?
            .filter_map(|r| r.ok())
            .collect();
        (total, entries)
    };

    Ok(Json(HistoryResponse {
        entries,
        total,
        limit,
        offset,
    }))
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
