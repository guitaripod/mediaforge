use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_util::io::ReaderStream;
use tracing::error;

use crate::api::error::AppResult;
use crate::api::AppState;
use crate::metadata::TmdbClient;
use crate::scanner::Scanner;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/metadata/scan", post(trigger_scan))
        .route("/api/metadata/refresh", post(trigger_refresh))
        .route("/api/metadata/image/{*path}", get(proxy_image))
}

async fn trigger_scan(State(state): State<Arc<AppState>>) -> AppResult<Json<serde_json::Value>> {
    let db = state.db.clone();
    let ffmpeg = state.ffmpeg.clone();
    let tmdb = state.tmdb.clone();
    let dirs = state.config.library.media_dirs.clone();
    let status = state.scan_status.clone();

    tokio::spawn(async move {
        status.start_scan();
        let scanner = Scanner::new(db.clone(), ffmpeg);
        match scanner.scan_directories(&dirs).await {
            Ok(count) => {
                status.set_items_found(count);
                if tmdb.has_key() {
                    status.start_metadata();
                    let _ = tmdb.migrate_numeric_genres(&db).await;
                    let _ = tmdb.update_movie_metadata(&db).await;
                    let _ = tmdb.update_tv_metadata(&db).await;
                }
                status.finish();
            }
            Err(e) => {
                error!("Library scan failed: {}", e);
                status.fail(e.to_string());
            }
        }
    });

    Ok(Json(serde_json::json!({ "status": "scan_started" })))
}

async fn trigger_refresh(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<serde_json::Value>> {
    let tmdb = state.tmdb.clone();
    let db = state.db.clone();
    let status = state.scan_status.clone();

    tokio::spawn(async move {
        status.start_metadata();
        if let Err(e) = tmdb.migrate_numeric_genres(&db).await {
            error!("Genre migration failed: {}", e);
        }
        if let Err(e) = tmdb.update_movie_metadata(&db).await {
            error!("Movie metadata refresh failed: {}", e);
        }
        if let Err(e) = tmdb.update_tv_metadata(&db).await {
            error!("TV metadata refresh failed: {}", e);
        }
        status.finish();
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

    let tmp_path = cache_path.with_extension("tmp");
    tokio::fs::write(&tmp_path, &data).await?;
    if let Err(e) = tokio::fs::rename(&tmp_path, cache_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(e.into());
    }

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
