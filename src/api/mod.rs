pub mod routes;

use axum::response::Json;
use axum::routing::get;
use axum::Router;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::db::Database;
use crate::ffmpeg::FFmpeg;
use crate::hls::HlsManager;
use crate::metadata::TmdbClient;

pub struct AppState {
    pub db: Database,
    pub ffmpeg: FFmpeg,
    pub hls: HlsManager,
    pub tmdb: TmdbClient,
    pub config: Config,
    pub image_fetches: dashmap::DashMap<String, tokio::sync::broadcast::Sender<()>>,
}

async fn api_index() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "MediaForge",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "library": {
                "movies": "/api/library/movies",
                "shows": "/api/library/shows",
                "recent": "/api/library/recent",
                "search": "/api/library/search?q={query}",
            },
            "playback": {
                "state": "/api/playback/{id}/state",
                "watched": "/api/playback/{id}/watched",
            },
            "streaming": {
                "info": "/api/stream/{id}/info",
                "hls_prepare": "/api/stream/{id}/hls/prepare",
                "hls_playlist": "/api/stream/{id}/hls/playlist.m3u8",
                "direct": "/api/stream/{id}/direct",
                "subtitle": "/api/stream/{id}/subtitle/{sub_id}",
            },
            "metadata": {
                "scan": "/api/metadata/scan",
                "refresh": "/api/metadata/refresh",
                "image": "/api/metadata/image/{path}?size={w92|w185|w342|w500|w780|original}",
            },
            "system": {
                "health": "/api/system/health",
                "stats": "/api/system/stats",
                "config": "/api/system/config",
            },
        },
    }))
}

pub fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(api_index))
        .merge(routes::library_routes())
        .merge(routes::playback_routes())
        .merge(routes::streaming_routes())
        .merge(routes::metadata_routes())
        .merge(routes::system_routes())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(state))
}
