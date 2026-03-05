use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{Json, Response};
use axum::routing::get;
use axum::Router;
use tracing::debug;

use crate::api::error::AppResult;
use crate::api::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/system/health", get(health))
        .route("/api/system/stats", get(stats))
        .route("/api/system/config", get(get_config))
        .route("/api/system/scan-status", get(scan_status))
        .route("/api/system/ws", get(scan_status_ws))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn stats(State(state): State<Arc<AppState>>) -> AppResult<Json<serde_json::Value>> {
    let conn = state.db.conn();

    let (movie_count, episode_count, total_size, total_duration): (i64, i64, i64, f64) =
        conn.query_row(
            "SELECT
                COUNT(CASE WHEN media_type = 'movie' THEN 1 END),
                COUNT(CASE WHEN media_type = 'episode' THEN 1 END),
                COALESCE(SUM(file_size), 0),
                COALESCE(SUM(duration_secs), 0)
             FROM media_items",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

    let show_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM tv_shows", [], |row| row.get(0))?;

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

async fn scan_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.scan_status.to_json())
}

async fn scan_status_ws(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| handle_scan_ws(socket, state))
}

async fn handle_scan_ws(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.scan_status.subscribe();

    let initial = state.scan_status.to_json();
    if socket
        .send(Message::Text(initial.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(json) => {
                        if socket.send(Message::Text(json.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        debug!("WebSocket client lagged by {} messages", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}
