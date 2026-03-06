use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

const MAX_SEARCH_LENGTH: usize = 200;

use crate::api::error::AppResult;
use crate::api::helpers::{get_audio_tracks_for_media, get_playback_state, get_subtitles_for_media, media_item_from_row, MEDIA_ITEM_COLUMNS};
use crate::api::AppState;
use crate::db::models::{AudioTrack, EpisodeSummary, MediaItem, MediaSummary, PlaybackState, Subtitle, TvShow, TvShowSummary};

pub fn routes() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::new()
        .routes(routes!(list_movies))
        .routes(routes!(get_movie))
        .routes(routes!(list_shows))
        .routes(routes!(get_show))
        .routes(routes!(get_season_episodes))
        .routes(routes!(next_episode))
        .routes(routes!(get_episode))
        .routes(routes!(continue_watching))
        .routes(routes!(on_deck))
        .routes(routes!(recently_watched))
        .routes(routes!(recent_items))
        .routes(routes!(list_genres))
        .routes(routes!(random_item))
        .routes(routes!(search_library))
}

#[derive(Deserialize, utoipa::IntoParams)]
struct ListParams {
    page: Option<u32>,
    per_page: Option<u32>,
    sort: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
struct MovieListParams {
    page: Option<u32>,
    per_page: Option<u32>,
    sort: Option<String>,
    genre: Option<String>,
}

#[derive(Serialize)]
struct PaginatedResponse<T: Serialize> {
    items: Vec<T>,
    total: i64,
    page: u32,
    per_page: u32,
    total_pages: u32,
}

#[derive(Serialize, ToSchema)]
struct PaginatedMediaSummary {
    items: Vec<MediaSummary>,
    total: i64,
    page: u32,
    per_page: u32,
    total_pages: u32,
}

#[derive(Serialize, ToSchema)]
struct PaginatedTvShowSummary {
    items: Vec<TvShowSummary>,
    total: i64,
    page: u32,
    per_page: u32,
    total_pages: u32,
}

#[derive(Serialize, ToSchema)]
struct MediaDetailResponse {
    item: MediaItem,
    subtitles: Vec<Subtitle>,
    playback: Option<PlaybackState>,
    audio_tracks: Vec<AudioTrack>,
}

#[derive(Serialize, ToSchema)]
struct ShowDetailResponse {
    show: TvShow,
    seasons: Vec<SeasonSummary>,
}

#[derive(Serialize, ToSchema)]
struct SeasonSummary {
    season_number: i32,
    episode_count: i32,
    watched_count: i32,
}

#[derive(Serialize, ToSchema)]
struct OnDeckItem {
    show_id: String,
    show_name: String,
    poster_path: Option<String>,
    episode_id: String,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    episode_title: Option<String>,
    duration_secs: Option<f64>,
    position_secs: f64,
}

#[derive(Serialize, ToSchema)]
struct ContinueWatchingItem {
    id: String,
    title: String,
    media_type: String,
    poster_path: Option<String>,
    duration_secs: Option<f64>,
    position_secs: f64,
    progress_percent: Option<f64>,
    last_played_at: String,
    show_name: Option<String>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    episode_title: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct ContinueWatchingResponse {
    items: Vec<ContinueWatchingItem>,
    total: i64,
}

#[derive(Serialize, ToSchema)]
struct SearchResult {
    id: String,
    title: String,
    media_type: String,
    year: Option<i32>,
    poster_path: Option<String>,
    rating: Option<f64>,
    duration_secs: Option<f64>,
    video_width: Option<i32>,
    video_height: Option<i32>,
    hdr_format: Option<String>,
    show_name: Option<String>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    episode_title: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
struct SearchParams {
    q: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
struct RandomParams {
    media_type: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/library/movies",
    tag = "library",
    params(MovieListParams),
    responses(
        (status = 200, body = PaginatedMediaSummary),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn list_movies(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MovieListParams>,
) -> AppResult<Json<PaginatedResponse<MediaSummary>>> {
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

    let (total, movies) = if let Some(ref genre) = params.genre {
        let genre_pattern = format!("%{}%", genre);
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM media_items WHERE media_type = 'movie' AND genres LIKE ?1",
            [&genre_pattern],
            |row| row.get(0),
        )?;

        let query = format!(
            "SELECT id, title, year, poster_path, rating, duration_secs, video_width, video_height, hdr_format
             FROM media_items WHERE media_type = 'movie' AND genres LIKE ?1 ORDER BY {} LIMIT ?2 OFFSET ?3",
            order
        );
        let mut stmt = conn.prepare(&query)?;
        let movies: Vec<MediaSummary> = stmt
            .query_map(rusqlite::params![genre_pattern, per_page, offset], |row| {
                Ok(MediaSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    media_type: "movie".to_string(),
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
        (total, movies)
    } else {
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
        let movies: Vec<MediaSummary> = stmt
            .query_map(rusqlite::params![per_page, offset], |row| {
                Ok(MediaSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    media_type: "movie".to_string(),
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
        (total, movies)
    };

    let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;

    Ok(Json(PaginatedResponse {
        items: movies,
        total,
        page,
        per_page,
        total_pages,
    }))
}

#[utoipa::path(
    get,
    path = "/api/library/movies/{id}",
    tag = "library",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, body = MediaDetailResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn get_movie(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let conn = state.db.conn();
    let query = format!(
        "SELECT {} FROM media_items WHERE id = ?1 AND media_type = 'movie'",
        MEDIA_ITEM_COLUMNS
    );
    let item: Option<MediaItem> = conn.query_row(&query, [&id], media_item_from_row).ok();

    match item {
        Some(movie) => {
            let subtitles = get_subtitles_for_media(&conn, &movie.id)?;
            let playback = get_playback_state(&conn, &movie.id)?;
            let audio_tracks = get_audio_tracks_for_media(&conn, &movie.id)?;

            Ok(Json(MediaDetailResponse {
                item: movie,
                subtitles,
                playback,
                audio_tracks,
            })
            .into_response())
        }
        None => Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
            .into_response()),
    }
}

#[utoipa::path(
    get,
    path = "/api/library/shows",
    tag = "library",
    params(ListParams),
    responses(
        (status = 200, body = PaginatedTvShowSummary),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn list_shows(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> AppResult<Json<PaginatedResponse<TvShowSummary>>> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).min(200);
    let offset = (page - 1) * per_page;
    let conn = state.db.conn();

    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tv_shows",
        [],
        |row| row.get(0),
    )?;

    let order = match params.sort.as_deref() {
        Some("name") => "t.name ASC",
        Some("added") => "t.added_at DESC",
        Some("rating") => "t.rating DESC",
        _ => "t.name ASC",
    };

    let query = format!(
        "SELECT t.id, t.name, t.poster_path, t.rating, t.first_air_date,
                COUNT(DISTINCT m.season_number),
                COUNT(m.id),
                COALESCE(SUM(CASE WHEN p.is_watched = 1 THEN 1 ELSE 0 END), 0)
         FROM tv_shows t
         LEFT JOIN media_items m ON m.show_name = t.name AND m.media_type = 'episode'
         LEFT JOIN playback_state p ON m.id = p.media_id
         GROUP BY t.id
         ORDER BY {} LIMIT ?1 OFFSET ?2",
        order
    );

    let mut stmt = conn.prepare(&query)?;
    let shows: Vec<TvShowSummary> = stmt
        .query_map(rusqlite::params![per_page, offset], |row| {
            Ok(TvShowSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                poster_path: row.get(2)?,
                rating: row.get(3)?,
                first_air_date: row.get(4)?,
                season_count: row.get(5)?,
                episode_count: row.get(6)?,
                watched_count: row.get(7)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;

    Ok(Json(PaginatedResponse {
        items: shows,
        total,
        page,
        per_page,
        total_pages,
    }))
}

#[utoipa::path(
    get,
    path = "/api/library/shows/{id}",
    tag = "library",
    params(("id" = String, Path, description = "TV show ID")),
    responses(
        (status = 200, body = ShowDetailResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
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
                "SELECT m.season_number, COUNT(*),
                        COALESCE(SUM(CASE WHEN p.is_watched = 1 THEN 1 ELSE 0 END), 0)
                 FROM media_items m
                 LEFT JOIN playback_state p ON m.id = p.media_id
                 WHERE m.show_name = ?1 AND m.media_type = 'episode' AND m.season_number IS NOT NULL
                 GROUP BY m.season_number
                 ORDER BY m.season_number"
            )?;
            let seasons: Vec<SeasonSummary> = stmt
                .query_map([&show.name], |row| {
                    Ok(SeasonSummary {
                        season_number: row.get(0)?,
                        episode_count: row.get(1)?,
                        watched_count: row.get(2)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            Ok(Json(ShowDetailResponse {
                show,
                seasons,
            })
            .into_response())
        }
        None => Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
            .into_response()),
    }
}

#[utoipa::path(
    get,
    path = "/api/library/shows/{id}/seasons/{season}",
    tag = "library",
    params(
        ("id" = String, Path, description = "TV show ID"),
        ("season" = i32, Path, description = "Season number"),
    ),
    responses(
        (status = 200, body = Vec<EpisodeSummary>),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
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

#[utoipa::path(
    get,
    path = "/api/library/shows/{id}/next",
    tag = "library",
    params(("id" = String, Path, description = "TV show ID")),
    responses(
        (status = 200, body = Option<EpisodeSummary>),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
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
             ORDER BY (CASE WHEN COALESCE(p.position_secs, 0) > 0 THEN 0 ELSE 1 END),
                      m.season_number ASC, m.episode_number ASC
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

    Ok(Json(next).into_response())
}

#[utoipa::path(
    get,
    path = "/api/library/episodes/{id}",
    tag = "library",
    params(("id" = String, Path, description = "Media item ID")),
    responses(
        (status = 200, body = MediaDetailResponse),
        (status = 404, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
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
            let audio_tracks = get_audio_tracks_for_media(&conn, &episode.id)?;

            Ok(Json(MediaDetailResponse {
                item: episode,
                subtitles,
                playback,
                audio_tracks,
            })
            .into_response())
        }
        None => Ok((StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "Not found" })))
            .into_response()),
    }
}

#[utoipa::path(
    get,
    path = "/api/library/continue",
    tag = "library",
    params(ListParams),
    responses(
        (status = 200, body = ContinueWatchingResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn continue_watching(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> AppResult<Json<ContinueWatchingResponse>> {
    let limit = params.per_page.unwrap_or(20).min(100);
    let conn = state.db.conn();

    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM playback_state p
         JOIN media_items m ON m.id = p.media_id
         WHERE p.is_watched = 0 AND p.position_secs > 0",
        [],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT m.id, m.title, m.media_type, COALESCE(m.poster_path, t.poster_path), m.duration_secs,
                p.position_secs, p.last_played_at,
                m.show_name, m.season_number, m.episode_number, m.episode_title
         FROM playback_state p
         JOIN media_items m ON m.id = p.media_id
         LEFT JOIN tv_shows t ON m.show_name = t.name AND m.media_type = 'episode'
         WHERE p.is_watched = 0 AND p.position_secs > 0
         ORDER BY p.last_played_at DESC
         LIMIT ?1",
    )?;

    let items: Vec<ContinueWatchingItem> = stmt
        .query_map([limit], |row| {
            let duration_secs: Option<f64> = row.get(4)?;
            let position_secs: f64 = row.get(5)?;
            let progress_percent = duration_secs
                .filter(|&d| d > 0.0)
                .map(|d| (position_secs / d * 100.0).min(100.0));
            Ok(ContinueWatchingItem {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: row.get(2)?,
                poster_path: row.get(3)?,
                duration_secs,
                position_secs,
                progress_percent,
                last_played_at: row.get(6)?,
                show_name: row.get(7)?,
                season_number: row.get(8)?,
                episode_number: row.get(9)?,
                episode_title: row.get(10)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(ContinueWatchingResponse { items, total }))
}

#[utoipa::path(
    get,
    path = "/api/library/ondeck",
    tag = "library",
    params(ListParams),
    responses(
        (status = 200, body = Vec<OnDeckItem>),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn on_deck(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<OnDeckItem>>> {
    let limit = params.per_page.unwrap_or(20).min(100);
    let conn = state.db.conn();

    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.poster_path,
                next_ep.id, next_ep.season_number, next_ep.episode_number, next_ep.episode_title,
                next_ep.duration_secs, COALESCE(p.position_secs, 0)
         FROM tv_shows t
         JOIN media_items next_ep ON next_ep.id = (
             SELECT m.id FROM media_items m
             LEFT JOIN playback_state ps ON m.id = ps.media_id
             WHERE m.show_name = t.name AND m.media_type = 'episode'
               AND m.season_number IS NOT NULL AND m.episode_number IS NOT NULL
               AND COALESCE(ps.is_watched, 0) = 0
             ORDER BY (CASE WHEN COALESCE(ps.position_secs, 0) > 0 THEN 0 ELSE 1 END),
                      m.season_number ASC, m.episode_number ASC
             LIMIT 1
         )
         LEFT JOIN playback_state p ON next_ep.id = p.media_id
         WHERE EXISTS (
             SELECT 1 FROM playback_state ps2
             JOIN media_items m2 ON m2.id = ps2.media_id
             WHERE m2.show_name = t.name AND m2.media_type = 'episode'
               AND (ps2.is_watched = 1 OR ps2.position_secs > 0)
         )
         ORDER BY COALESCE(p.last_played_at, t.added_at) DESC
         LIMIT ?1"
    )?;

    let items: Vec<OnDeckItem> = stmt
        .query_map([limit], |row| {
            Ok(OnDeckItem {
                show_id: row.get(0)?,
                show_name: row.get(1)?,
                poster_path: row.get(2)?,
                episode_id: row.get(3)?,
                season_number: row.get(4)?,
                episode_number: row.get(5)?,
                episode_title: row.get(6)?,
                duration_secs: row.get(7)?,
                position_secs: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(items))
}

#[utoipa::path(
    get,
    path = "/api/library/watched",
    tag = "library",
    params(ListParams),
    responses(
        (status = 200, body = Vec<MediaSummary>),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn recently_watched(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<MediaSummary>>> {
    let limit = params.per_page.unwrap_or(20).min(100);
    let conn = state.db.conn();

    let mut stmt = conn.prepare(
        "SELECT m.id, m.title, m.media_type, m.year, COALESCE(m.poster_path, t.poster_path),
                m.rating, m.duration_secs, m.video_width, m.video_height, m.hdr_format
         FROM playback_state p
         JOIN media_items m ON m.id = p.media_id
         LEFT JOIN tv_shows t ON m.show_name = t.name AND m.media_type = 'episode'
         WHERE p.is_watched = 1
         ORDER BY p.last_played_at DESC
         LIMIT ?1"
    )?;

    let items: Vec<MediaSummary> = stmt
        .query_map([limit], |row| {
            Ok(MediaSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: row.get(2)?,
                year: row.get(3)?,
                poster_path: row.get(4)?,
                rating: row.get(5)?,
                duration_secs: row.get(6)?,
                video_width: row.get(7)?,
                video_height: row.get(8)?,
                hdr_format: row.get(9)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(items))
}

#[utoipa::path(
    get,
    path = "/api/library/recent",
    tag = "library",
    params(ListParams),
    responses(
        (status = 200, body = Vec<MediaSummary>),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn recent_items(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> AppResult<Json<Vec<MediaSummary>>> {
    let limit = params.per_page.unwrap_or(20).min(100);
    let conn = state.db.conn();

    let mut stmt = conn.prepare(
        "SELECT m.id, m.title, m.media_type, m.year, COALESCE(m.poster_path, t.poster_path),
                m.rating, m.duration_secs, m.video_width, m.video_height, m.hdr_format
         FROM media_items m
         LEFT JOIN tv_shows t ON m.show_name = t.name AND m.media_type = 'episode'
         ORDER BY m.added_at DESC LIMIT ?1",
    )?;

    let items: Vec<MediaSummary> = stmt
        .query_map([limit], |row| {
            Ok(MediaSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: row.get(2)?,
                year: row.get(3)?,
                poster_path: row.get(4)?,
                rating: row.get(5)?,
                duration_secs: row.get(6)?,
                video_width: row.get(7)?,
                video_height: row.get(8)?,
                hdr_format: row.get(9)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(items))
}

#[utoipa::path(
    get,
    path = "/api/library/genres",
    tag = "library",
    responses(
        (status = 200, body = Vec<String>),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn list_genres(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<Vec<String>>> {
    let conn = state.db.conn();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT genres FROM media_items WHERE genres IS NOT NULL AND genres != ''"
    )?;

    let mut genre_set = std::collections::BTreeSet::new();
    let rows: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    for genres_str in &rows {
        for genre in genres_str.split(',') {
            let trimmed = genre.trim();
            if !trimmed.is_empty() {
                genre_set.insert(trimmed.to_string());
            }
        }
    }

    Ok(Json(genre_set.into_iter().collect()))
}

#[utoipa::path(
    get,
    path = "/api/library/random",
    tag = "library",
    params(RandomParams),
    responses(
        (status = 200, body = Option<MediaSummary>),
        (status = 400, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
async fn random_item(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RandomParams>,
) -> AppResult<Response> {
    let conn = state.db.conn();

    let query = match params.media_type.as_deref() {
        Some("movie") =>
            "SELECT id, title, media_type, year, poster_path, rating, duration_secs, video_width, video_height, hdr_format
             FROM media_items WHERE media_type = 'movie' ORDER BY RANDOM() LIMIT 1",
        Some("episode") =>
            "SELECT id, title, media_type, year, poster_path, rating, duration_secs, video_width, video_height, hdr_format
             FROM media_items WHERE media_type = 'episode' ORDER BY RANDOM() LIMIT 1",
        Some("unwatched") =>
            "SELECT m.id, m.title, m.media_type, m.year, COALESCE(m.poster_path, t.poster_path), m.rating, m.duration_secs, m.video_width, m.video_height, m.hdr_format
             FROM media_items m
             LEFT JOIN tv_shows t ON m.show_name = t.name AND m.media_type = 'episode'
             LEFT JOIN playback_state p ON m.id = p.media_id
             WHERE COALESCE(p.is_watched, 0) = 0
             ORDER BY RANDOM() LIMIT 1",
        None =>
            "SELECT m.id, m.title, m.media_type, m.year, COALESCE(m.poster_path, t.poster_path), m.rating, m.duration_secs, m.video_width, m.video_height, m.hdr_format
             FROM media_items m
             LEFT JOIN tv_shows t ON m.show_name = t.name AND m.media_type = 'episode'
             ORDER BY RANDOM() LIMIT 1",
        Some(_) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "media_type must be 'movie', 'episode', or 'unwatched'" })),
            ).into_response());
        }
    };

    let item: Option<MediaSummary> = conn
        .query_row(query, [], |row| {
            Ok(MediaSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: row.get(2)?,
                year: row.get(3)?,
                poster_path: row.get(4)?,
                rating: row.get(5)?,
                duration_secs: row.get(6)?,
                video_width: row.get(7)?,
                video_height: row.get(8)?,
                hdr_format: row.get(9)?,
            })
        })
        .ok();

    Ok(Json(item).into_response())
}

#[utoipa::path(
    get,
    path = "/api/library/search",
    tag = "library",
    params(SearchParams),
    responses(
        (status = 200, body = Vec<SearchResult>),
        (status = 400, body = crate::api::error::ErrorResponse),
        (status = 500, body = crate::api::error::ErrorResponse),
    ),
)]
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
    let pattern = format!("%{}%", escaped);

    let mut show_stmt = conn.prepare(
        "SELECT t.id, t.name, t.poster_path, t.rating, t.first_air_date
         FROM tv_shows t
         WHERE t.name LIKE ?1 ESCAPE '\\'
         ORDER BY t.name LIMIT 10",
    )?;
    let shows: Vec<SearchResult> = show_stmt
        .query_map([&pattern], |row| {
            let first_air_date: Option<String> = row.get(4)?;
            let year = first_air_date
                .as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse::<i32>().ok());
            Ok(SearchResult {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: "show".to_string(),
                year,
                poster_path: row.get(2)?,
                rating: row.get(3)?,
                duration_secs: None,
                video_width: None,
                video_height: None,
                hdr_format: None,
                show_name: None,
                season_number: None,
                episode_number: None,
                episode_title: None,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let media_limit = 50 - shows.len().min(50) as u32;
    let mut media_stmt = conn.prepare(
        "SELECT m.id, m.title, m.media_type, m.year, COALESCE(m.poster_path, t.poster_path),
                m.rating, m.duration_secs, m.video_width, m.video_height, m.hdr_format,
                m.show_name, m.season_number, m.episode_number, m.episode_title
         FROM media_items m
         LEFT JOIN tv_shows t ON m.show_name = t.name AND m.media_type = 'episode'
         WHERE m.title LIKE ?1 ESCAPE '\\' OR m.show_name LIKE ?1 ESCAPE '\\' OR m.episode_title LIKE ?1 ESCAPE '\\'
         ORDER BY m.sort_title LIMIT ?2",
    )?;

    let media: Vec<SearchResult> = media_stmt
        .query_map(rusqlite::params![pattern, media_limit], |row| {
            Ok(SearchResult {
                id: row.get(0)?,
                title: row.get(1)?,
                media_type: row.get(2)?,
                year: row.get(3)?,
                poster_path: row.get(4)?,
                rating: row.get(5)?,
                duration_secs: row.get(6)?,
                video_width: row.get(7)?,
                video_height: row.get(8)?,
                hdr_format: row.get(9)?,
                show_name: row.get(10)?,
                season_number: row.get(11)?,
                episode_number: row.get(12)?,
                episode_title: row.get(13)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut results = shows;
    results.extend(media);

    Ok(Json(results).into_response())
}
