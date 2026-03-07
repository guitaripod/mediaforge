use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::api::error::AppResult;
use crate::api::helpers::get_playback_state;
use crate::api::AppState;
use crate::db::models::{ActivityLogEntry, PlaybackState};

fn media_exists(conn: &rusqlite::Connection, id: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM media_items WHERE id = ?1",
        [id],
        |_| Ok(()),
    )
    .is_ok()
}

#[derive(Serialize, ToSchema)]
struct UpdatedCount {
    updated: usize,
}

pub fn routes() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::new()
        .routes(routes!(get_playback, update_playback))
        .routes(routes!(mark_watched, mark_unwatched))
        .routes(routes!(mark_show_watched, mark_show_unwatched))
        .routes(routes!(mark_season_watched, mark_season_unwatched))
        .routes(routes!(get_activity_history))
}

#[utoipa::path(
    get,
    path = "/api/playback/{id}/state",
    tag = "playback",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, description = "Playback state", body = PlaybackState),
        (status = 404, description = "Not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn get_playback(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    if !media_exists(&conn, &id) {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    }
    let ps = get_playback_state(&conn, &id)?;
    Ok(Json(ps.unwrap_or(PlaybackState {
        media_id: id,
        position_secs: 0.0,
        is_watched: false,
        last_played_at: None,
    }))
    .into_response())
}

#[derive(Deserialize, ToSchema)]
struct UpdatePlaybackRequest {
    position_secs: f64,
    event: Option<String>,
}

#[utoipa::path(
    put,
    path = "/api/playback/{id}/state",
    tag = "playback",
    params(("id" = String, Path, description = "Media item ID")),
    request_body = UpdatePlaybackRequest,
    responses(
        (status = 204, description = "Playback state updated"),
        (status = 400, description = "Invalid request", body = crate::api::error::ErrorResponse),
        (status = 404, description = "Not found", body = crate::api::error::ErrorResponse),
    )
)]
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

    let conn = state.db.conn();
    if !media_exists(&conn, &id) {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    }
    let duration: Option<f64> = conn
        .query_row(
            "SELECT duration_secs FROM media_items WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap_or(None);

    let auto_watched = duration
        .filter(|&d| d > 0.0)
        .is_some_and(|d| body.position_secs / d >= 0.9);

    if auto_watched {
        conn.execute(
            "INSERT INTO playback_state (media_id, position_secs, is_watched, last_played_at)
             VALUES (?1, ?2, 1, datetime('now'))
             ON CONFLICT(media_id) DO UPDATE SET position_secs = ?2, is_watched = 1, last_played_at = datetime('now')",
            rusqlite::params![id, body.position_secs],
        )?;
    } else {
        conn.execute(
            "INSERT INTO playback_state (media_id, position_secs, last_played_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(media_id) DO UPDATE SET position_secs = ?2, last_played_at = datetime('now')",
            rusqlite::params![id, body.position_secs],
        )?;
    }

    let log_event = if auto_watched { "complete" } else { event_type };

    if log_event == "position_update" {
        conn.execute(
            "INSERT INTO activity_log (media_id, event_type, position_secs)
             SELECT ?1, ?2, ?3
             WHERE NOT EXISTS (
                 SELECT 1 FROM activity_log
                 WHERE media_id = ?1 AND event_type = 'position_update'
                   AND created_at > datetime('now', '-30 seconds')
             )",
            rusqlite::params![id, log_event, body.position_secs],
        )?;
    } else {
        conn.execute(
            "INSERT INTO activity_log (media_id, event_type, position_secs) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, log_event, body.position_secs],
        )?;
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    post,
    path = "/api/playback/{id}/watched",
    tag = "playback",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 204, description = "Marked as watched"),
        (status = 404, description = "Not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn mark_watched(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    if !media_exists(&conn, &id) {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    }
    conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         VALUES (?1, 1, 0, datetime('now'))
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 1, position_secs = 0, last_played_at = datetime('now')",
        [&id],
    )?;
    conn.execute(
        "INSERT INTO activity_log (media_id, event_type, position_secs) VALUES (?1, 'complete', 0)",
        [&id],
    )?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    delete,
    path = "/api/playback/{id}/watched",
    tag = "playback",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 204, description = "Marked as unwatched"),
        (status = 404, description = "Not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn mark_unwatched(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    if !media_exists(&conn, &id) {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" }))).into_response());
    }
    conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         VALUES (?1, 0, 0, datetime('now'))
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 0, position_secs = 0, last_played_at = datetime('now')",
        [&id],
    )?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
struct HistoryParams {
    media_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

#[derive(Serialize, ToSchema)]
struct HistoryResponse {
    entries: Vec<ActivityLogEntry>,
    total: i64,
    limit: u32,
    offset: u32,
}

#[utoipa::path(
    get,
    path = "/api/playback/history",
    tag = "playback",
    params(HistoryParams),
    responses(
        (status = 200, description = "Activity history", body = HistoryResponse),
    )
)]
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

fn show_name_for_id(conn: &rusqlite::Connection, show_id: &str) -> Option<String> {
    conn.query_row("SELECT name FROM tv_shows WHERE id = ?1", [show_id], |row| row.get(0)).ok()
}

#[utoipa::path(
    post,
    path = "/api/playback/shows/{id}/watched",
    tag = "playback",
    params(("id" = String, Path, description = "TV show ID")),
    responses(
        (status = 200, description = "All episodes marked as watched", body = UpdatedCount),
        (status = 404, description = "Show not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn mark_show_watched(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    let Some(show_name) = show_name_for_id(&conn, &id) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Show not found" }))).into_response());
    };
    let count = conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         SELECT id, 1, 0, datetime('now') FROM media_items WHERE show_name = ?1 AND media_type = 'episode'
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 1, position_secs = 0, last_played_at = datetime('now')",
        [&show_name],
    )?;
    Ok(Json(serde_json::json!({ "updated": count })).into_response())
}

#[utoipa::path(
    delete,
    path = "/api/playback/shows/{id}/watched",
    tag = "playback",
    params(("id" = String, Path, description = "TV show ID")),
    responses(
        (status = 200, description = "All episodes marked as unwatched", body = UpdatedCount),
        (status = 404, description = "Show not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn mark_show_unwatched(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    let Some(show_name) = show_name_for_id(&conn, &id) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Show not found" }))).into_response());
    };
    let count = conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         SELECT id, 0, 0, datetime('now') FROM media_items WHERE show_name = ?1 AND media_type = 'episode'
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 0, position_secs = 0, last_played_at = datetime('now')",
        [&show_name],
    )?;
    Ok(Json(serde_json::json!({ "updated": count })).into_response())
}

#[utoipa::path(
    post,
    path = "/api/playback/shows/{id}/seasons/{season}/watched",
    tag = "playback",
    params(
        ("id" = String, Path, description = "TV show ID"),
        ("season" = i32, Path, description = "Season number"),
    ),
    responses(
        (status = 200, description = "Season episodes marked as watched", body = UpdatedCount),
        (status = 404, description = "Show not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn mark_season_watched(
    State(state): State<Arc<AppState>>,
    Path((id, season)): Path<(String, i32)>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    let Some(show_name) = show_name_for_id(&conn, &id) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Show not found" }))).into_response());
    };
    let count = conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         SELECT id, 1, 0, datetime('now') FROM media_items WHERE show_name = ?1 AND media_type = 'episode' AND COALESCE(season_number, 1) = ?2
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 1, position_secs = 0, last_played_at = datetime('now')",
        rusqlite::params![show_name, season],
    )?;
    Ok(Json(serde_json::json!({ "updated": count })).into_response())
}

#[utoipa::path(
    delete,
    path = "/api/playback/shows/{id}/seasons/{season}/watched",
    tag = "playback",
    params(
        ("id" = String, Path, description = "TV show ID"),
        ("season" = i32, Path, description = "Season number"),
    ),
    responses(
        (status = 200, description = "Season episodes marked as unwatched", body = UpdatedCount),
        (status = 404, description = "Show not found", body = crate::api::error::ErrorResponse),
    )
)]
async fn mark_season_unwatched(
    State(state): State<Arc<AppState>>,
    Path((id, season)): Path<(String, i32)>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    let Some(show_name) = show_name_for_id(&conn, &id) else {
        return Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Show not found" }))).into_response());
    };
    let count = conn.execute(
        "INSERT INTO playback_state (media_id, is_watched, position_secs, last_played_at)
         SELECT id, 0, 0, datetime('now') FROM media_items WHERE show_name = ?1 AND media_type = 'episode' AND COALESCE(season_number, 1) = ?2
         ON CONFLICT(media_id) DO UPDATE SET is_watched = 0, position_secs = 0, last_played_at = datetime('now')",
        rusqlite::params![show_name, season],
    )?;
    Ok(Json(serde_json::json!({ "updated": count })).into_response())
}
