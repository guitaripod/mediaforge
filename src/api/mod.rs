mod error;
mod helpers;
mod routes;

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
    pub scan_status: Arc<ScanStatus>,
}

pub struct ScanStatus {
    inner: std::sync::Mutex<ScanStatusInner>,
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
}

struct ScanStatusInner {
    current: ScanPhase,
    last_completed_at: Option<String>,
    last_error: Option<String>,
}

enum ScanPhase {
    Idle,
    Scanning { started_at: String, items_found: u32 },
    FetchingMetadata { started_at: String, items_found: u32 },
}

impl ScanStatus {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            inner: std::sync::Mutex::new(ScanStatusInner {
                current: ScanPhase::Idle,
                last_completed_at: None,
                last_error: None,
            }),
            tx,
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<serde_json::Value> {
        self.tx.subscribe()
    }

    pub fn start_scan(&self) {
        let snapshot = {
            let mut s = self.inner.lock().unwrap();
            s.current = ScanPhase::Scanning {
                started_at: chrono::Utc::now().to_rfc3339(),
                items_found: 0,
            };
            s.last_error = None;
            Self::build_json(&s)
        };
        let _ = self.tx.send(snapshot);
    }

    pub fn set_items_found(&self, count: u32) {
        let snapshot = {
            let mut s = self.inner.lock().unwrap();
            match &mut s.current {
                ScanPhase::Scanning { items_found, .. }
                | ScanPhase::FetchingMetadata { items_found, .. } => *items_found = count,
                ScanPhase::Idle => {}
            }
            Self::build_json(&s)
        };
        let _ = self.tx.send(snapshot);
    }

    pub fn start_metadata(&self) {
        let snapshot = {
            let mut s = self.inner.lock().unwrap();
            let (started, found) = match &s.current {
                ScanPhase::Scanning { started_at, items_found } => {
                    (started_at.clone(), *items_found)
                }
                _ => (chrono::Utc::now().to_rfc3339(), 0),
            };
            s.current = ScanPhase::FetchingMetadata {
                started_at: started,
                items_found: found,
            };
            Self::build_json(&s)
        };
        let _ = self.tx.send(snapshot);
    }

    pub fn finish(&self) {
        let snapshot = {
            let mut s = self.inner.lock().unwrap();
            s.current = ScanPhase::Idle;
            s.last_completed_at = Some(chrono::Utc::now().to_rfc3339());
            Self::build_json(&s)
        };
        let _ = self.tx.send(snapshot);
    }

    pub fn fail(&self, error: String) {
        let snapshot = {
            let mut s = self.inner.lock().unwrap();
            s.current = ScanPhase::Idle;
            s.last_completed_at = Some(chrono::Utc::now().to_rfc3339());
            s.last_error = Some(error);
            Self::build_json(&s)
        };
        let _ = self.tx.send(snapshot);
    }

    fn build_json(s: &ScanStatusInner) -> serde_json::Value {
        match &s.current {
            ScanPhase::Idle => serde_json::json!({
                "status": "idle",
                "is_running": false,
                "last_completed_at": s.last_completed_at,
                "last_error": s.last_error,
            }),
            ScanPhase::Scanning { started_at, items_found } => serde_json::json!({
                "status": "scanning",
                "is_running": true,
                "started_at": started_at,
                "items_found": items_found,
            }),
            ScanPhase::FetchingMetadata { started_at, items_found } => serde_json::json!({
                "status": "fetching_metadata",
                "is_running": true,
                "started_at": started_at,
                "items_found": items_found,
            }),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let s = self.inner.lock().unwrap();
        Self::build_json(&s)
    }
}

async fn api_index() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "MediaForge",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "library": {
                "movies": "/api/library/movies",
                "shows": "/api/library/shows",
                "episodes": "/api/library/episodes/{id}",
                "continue": "/api/library/continue",
                "recent": "/api/library/recent",
                "search": "/api/library/search?q={query}",
                "next_episode": "/api/library/shows/{id}/next",
            },
            "playback": {
                "state": "/api/playback/{id}/state",
                "watched": "/api/playback/{id}/watched",
                "history": "/api/playback/history?media_id={id}&limit=50&offset=0",
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
                "scan_status": "/api/system/scan-status",
                "ws": "/api/system/ws",
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
        .merge(routes::library::routes())
        .merge(routes::playback::routes())
        .merge(routes::streaming::routes())
        .merge(routes::metadata::routes())
        .merge(routes::system::routes())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_status_starts_idle() {
        let status = ScanStatus::new();
        let json = status.to_json();
        assert_eq!(json["status"], "idle");
        assert_eq!(json["is_running"], false);
    }

    #[test]
    fn scan_status_scanning_phase() {
        let status = ScanStatus::new();
        status.start_scan();
        let json = status.to_json();
        assert_eq!(json["status"], "scanning");
        assert_eq!(json["is_running"], true);
        assert_eq!(json["items_found"], 0);
        assert!(json["started_at"].is_string());
    }

    #[test]
    fn scan_status_items_found_updates() {
        let status = ScanStatus::new();
        status.start_scan();
        status.set_items_found(42);
        let json = status.to_json();
        assert_eq!(json["items_found"], 42);
    }

    #[test]
    fn scan_status_metadata_preserves_context() {
        let status = ScanStatus::new();
        status.start_scan();
        status.set_items_found(10);
        status.start_metadata();
        let json = status.to_json();
        assert_eq!(json["status"], "fetching_metadata");
        assert_eq!(json["items_found"], 10);
        assert!(json["started_at"].is_string());
    }

    #[test]
    fn scan_status_finish_returns_to_idle() {
        let status = ScanStatus::new();
        status.start_scan();
        status.finish();
        let json = status.to_json();
        assert_eq!(json["status"], "idle");
        assert_eq!(json["is_running"], false);
        assert!(json["last_completed_at"].is_string());
        assert!(json["last_error"].is_null());
    }

    #[test]
    fn scan_status_fail_records_error() {
        let status = ScanStatus::new();
        status.start_scan();
        status.fail("disk full".into());
        let json = status.to_json();
        assert_eq!(json["status"], "idle");
        assert_eq!(json["last_error"], "disk full");
        assert!(json["last_completed_at"].is_string());
    }

    #[test]
    fn scan_status_new_scan_clears_error() {
        let status = ScanStatus::new();
        status.start_scan();
        status.fail("error".into());
        status.start_scan();
        let json = status.to_json();
        assert_eq!(json["status"], "scanning");
    }
}
