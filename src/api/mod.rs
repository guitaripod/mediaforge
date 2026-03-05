pub mod routes;

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

pub fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .merge(routes::library_routes())
        .merge(routes::playback_routes())
        .merge(routes::streaming_routes())
        .merge(routes::metadata_routes())
        .merge(routes::system_routes())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(state))
}
