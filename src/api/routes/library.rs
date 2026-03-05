use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};

const MAX_SEARCH_LENGTH: usize = 200;

use crate::api::error::AppResult;
use crate::api::helpers::{get_playback_state, get_subtitles_for_media, media_item_from_row, MEDIA_ITEM_COLUMNS};
use crate::api::AppState;
use crate::db::models::{EpisodeSummary, MediaItem, MovieSummary, TvShow, TvShowSummary};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/library/movies", get(list_movies))
        .route("/api/library/movies/{id}", get(get_movie))
        .route("/api/library/shows", get(list_shows))
        .route("/api/library/shows/{id}", get(get_show))
        .route(
            "/api/library/shows/{id}/seasons/{season}",
            get(get_season_episodes),
        )
        .route("/api/library/shows/{id}/next", get(next_episode))
        .route("/api/library/episodes/{id}", get(get_episode))
        .route("/api/library/continue", get(continue_watching))
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
    let query = format!("SELECT {} FROM media_items WHERE id = ?1", MEDIA_ITEM_COLUMNS);
    let item: Option<MediaItem> = conn.query_row(&query, [&id], media_item_from_row).ok();

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
                COUNT(DISTINCT m.season_number),
                COUNT(m.id)
         FROM tv_shows t
         LEFT JOIN media_items m ON m.show_name = t.name AND m.media_type = 'episode'
         GROUP BY t.id
         ORDER BY t.name"
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

async fn next_episode(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
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

    let next: Option<EpisodeSummary> = conn
        .query_row(
            "SELECT m.id, m.season_number, m.episode_number, m.episode_title, m.duration_secs,
                    COALESCE(p.is_watched, 0), COALESCE(p.position_secs, 0)
             FROM media_items m
             LEFT JOIN playback_state p ON m.id = p.media_id
             WHERE m.show_name = ?1 AND m.media_type = 'episode'
               AND m.season_number IS NOT NULL AND m.episode_number IS NOT NULL
               AND COALESCE(p.is_watched, 0) = 0
             ORDER BY m.season_number ASC, m.episode_number ASC
             LIMIT 1",
            [&show_name],
            |row| {
                Ok(EpisodeSummary {
                    id: row.get(0)?,
                    season_number: row.get(1)?,
                    episode_number: row.get(2)?,
                    episode_title: row.get(3)?,
                    duration_secs: row.get(4)?,
                    is_watched: row.get::<_, i32>(5)? != 0,
                    position_secs: row.get(6)?,
                })
            },
        )
        .ok();

    match next {
        Some(ep) => Ok(Json(ep).into_response()),
        None => Ok(Json(serde_json::json!({ "message": "All episodes watched" })).into_response()),
    }
}

async fn get_episode(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    let query = format!(
        "SELECT {} FROM media_items WHERE id = ?1 AND media_type = 'episode'",
        MEDIA_ITEM_COLUMNS
    );
    let item: Option<MediaItem> = conn.query_row(&query, [&id], media_item_from_row).ok();

    match item {
        Some(episode) => {
            let subtitles = get_subtitles_for_media(&conn, &episode.id)?;
            let playback = get_playback_state(&conn, &episode.id)?;

            Ok(Json(serde_json::json!({
                "item": episode,
                "subtitles": subtitles,
                "playback": playback,
            }))
            .into_response())
        }
        None => Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
            .into_response()),
    }
}

#[derive(Serialize)]
struct ContinueWatchingItem {
    id: String,
    title: String,
    media_type: String,
    poster_path: Option<String>,
    duration_secs: Option<f64>,
    position_secs: f64,
    last_played_at: String,
    show_name: Option<String>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    episode_title: Option<String>,
}

async fn continue_watching(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> AppResult<Json<Vec<ContinueWatchingItem>>> {
    let limit = params.per_page.unwrap_or(20).min(100);
    let conn = state.db.conn();

    let mut stmt = conn.prepare(
        "SELECT m.id, m.title, m.media_type, m.poster_path, m.duration_secs,
                p.position_secs, p.last_played_at,
                m.show_name, m.season_number, m.episode_number, m.episode_title
         FROM playback_state p
         JOIN media_items m ON m.id = p.media_id
         WHERE p.is_watched = 0 AND p.position_secs > 0
         ORDER BY p.last_played_at DESC
         LIMIT ?1",
    )?;

    let items: Vec<ContinueWatchingItem> = stmt
        .query_map([limit], |row| {
            Ok(ContinueWatchingItem {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: row.get(2)?,
                poster_path: row.get(3)?,
                duration_secs: row.get(4)?,
                position_secs: row.get(5)?,
                last_played_at: row.get(6)?,
                show_name: row.get(7)?,
                season_number: row.get(8)?,
                episode_number: row.get(9)?,
                episode_title: row.get(10)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(items))
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
) -> AppResult<Response> {
    if params.q.is_empty() || params.q.len() > MAX_SEARCH_LENGTH {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Query must be 1-{} characters", MAX_SEARCH_LENGTH) })),
        ).into_response());
    }

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

    Ok(Json(items).into_response())
}
