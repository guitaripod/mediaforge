use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::db::Database;

const TMDB_BASE_URL: &str = "https://api.themoviedb.org/3";
pub const TMDB_IMAGE_BASE: &str = "https://image.tmdb.org/t/p";

#[derive(Clone)]
pub struct TmdbClient {
    client: Client,
    api_key: String,
    language: String,
}

#[derive(Debug, Deserialize)]
struct SearchMovieResponse {
    results: Vec<TmdbMovie>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbMovie {
    id: i64,
    title: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    release_date: Option<String>,
    vote_average: Option<f64>,
    genre_ids: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
struct SearchTvResponse {
    results: Vec<TmdbTvShow>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbTvShow {
    id: i64,
    name: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    first_air_date: Option<String>,
    vote_average: Option<f64>,
    genre_ids: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbEpisode {
    name: Option<String>,
    overview: Option<String>,
    still_path: Option<String>,
    vote_average: Option<f64>,
    air_date: Option<String>,
}

impl TmdbClient {
    pub fn new(api_key: String, language: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            language,
        }
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    /// Fetch and update metadata for all movies missing TMDB data
    pub async fn update_movie_metadata(&self, db: &Database) -> Result<()> {
        if !self.has_key() {
            info!("No TMDB API key configured, skipping metadata fetch");
            return Ok(());
        }

        let movies: Vec<(String, String, Option<i32>)> = {
            let conn = db.conn();
            let mut stmt = conn.prepare(
                "SELECT id, title, year FROM media_items WHERE media_type = 'movie' AND tmdb_id IS NULL"
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        info!("Fetching TMDB metadata for {} movies", movies.len());

        for (id, title, year) in movies {
            match self.search_movie(&title, year).await {
                Ok(Some(movie)) => {
                    let genres = movie
                        .genre_ids
                        .as_ref()
                        .map(|ids| {
                            ids.iter()
                                .map(|id| id.to_string())
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .unwrap_or_default();

                    let conn = db.conn();
                    conn.execute(
                        "UPDATE media_items SET tmdb_id = ?1, overview = ?2, poster_path = ?3,
                         backdrop_path = ?4, genres = ?5, rating = ?6, release_date = ?7,
                         updated_at = datetime('now') WHERE id = ?8",
                        rusqlite::params![
                            movie.id,
                            movie.overview,
                            movie.poster_path,
                            movie.backdrop_path,
                            genres,
                            movie.vote_average,
                            movie.release_date,
                            id,
                        ],
                    )?;
                    debug!("Updated metadata for movie: {}", title);
                }
                Ok(None) => {
                    debug!("No TMDB result for: {}", title);
                }
                Err(e) => {
                    warn!("TMDB search failed for {}: {}", title, e);
                }
            }

            // Rate limit: TMDB allows ~40 requests per 10 seconds
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        Ok(())
    }

    /// Fetch and update metadata for all TV shows missing TMDB data
    pub async fn update_tv_metadata(&self, db: &Database) -> Result<()> {
        if !self.has_key() {
            return Ok(());
        }

        let shows: Vec<(String, String)> = {
            let conn = db.conn();
            let mut stmt =
                conn.prepare("SELECT id, name FROM tv_shows WHERE tmdb_id IS NULL")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };

        info!("Fetching TMDB metadata for {} TV shows", shows.len());

        for (id, name) in shows {
            match self.search_tv(&name).await {
                Ok(Some(show)) => {
                    let genres = show
                        .genre_ids
                        .as_ref()
                        .map(|ids| {
                            ids.iter()
                                .map(|id| id.to_string())
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .unwrap_or_default();

                    let tmdb_show_id = show.id;
                    {
                        let conn = db.conn();
                        conn.execute(
                            "UPDATE tv_shows SET tmdb_id = ?1, overview = ?2, poster_path = ?3,
                             backdrop_path = ?4, genres = ?5, rating = ?6, first_air_date = ?7
                             WHERE id = ?8",
                            rusqlite::params![
                                tmdb_show_id,
                                show.overview,
                                show.poster_path,
                                show.backdrop_path,
                                genres,
                                show.vote_average,
                                show.first_air_date,
                                id,
                            ],
                        )?;
                    }

                    self.update_episodes_for_show(db, &name, tmdb_show_id).await?;

                    debug!("Updated metadata for TV show: {}", name);
                }
                Ok(None) => debug!("No TMDB result for show: {}", name),
                Err(e) => warn!("TMDB search failed for show {}: {}", name, e),
            }

            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        Ok(())
    }

    async fn update_episodes_for_show(
        &self,
        db: &Database,
        show_name: &str,
        tmdb_show_id: i64,
    ) -> Result<()> {
        let episodes: Vec<(String, Option<i32>, Option<i32>)> = {
            let conn = db.conn();
            let mut stmt = conn.prepare(
                "SELECT id, season_number, episode_number FROM media_items
                 WHERE show_name = ?1 AND media_type = 'episode'"
            )?;
            let rows = stmt.query_map([show_name], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        for (id, season, episode) in episodes {
            if let (Some(s), Some(ep_num)) = (season, episode) {
                match self.get_episode(tmdb_show_id, s, ep_num).await {
                    Ok(Some(ep)) => {
                        let conn = db.conn();
                        conn.execute(
                            "UPDATE media_items SET episode_title = ?1, overview = ?2,
                             tmdb_id = ?3, rating = ?4, release_date = ?5,
                             updated_at = datetime('now') WHERE id = ?6",
                            rusqlite::params![
                                ep.name,
                                ep.overview,
                                tmdb_show_id,
                                ep.vote_average,
                                ep.air_date,
                                id,
                            ],
                        )?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        debug!("Failed to get episode S{:02}E{:02}: {}", s, ep_num, e);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }

        Ok(())
    }

    async fn search_movie(&self, title: &str, year: Option<i32>) -> Result<Option<TmdbMovie>> {
        let mut url = format!(
            "{}/search/movie?api_key={}&language={}&query={}",
            TMDB_BASE_URL,
            self.api_key,
            self.language,
            urlencoding(title),
        );
        if let Some(y) = year {
            url.push_str(&format!("&year={}", y));
        }

        let resp: SearchMovieResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.results.into_iter().next())
    }

    async fn search_tv(&self, name: &str) -> Result<Option<TmdbTvShow>> {
        let url = format!(
            "{}/search/tv?api_key={}&language={}&query={}",
            TMDB_BASE_URL,
            self.api_key,
            self.language,
            urlencoding(name),
        );

        let resp: SearchTvResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.results.into_iter().next())
    }

    async fn get_episode(
        &self,
        show_id: i64,
        season: i32,
        episode: i32,
    ) -> Result<Option<TmdbEpisode>> {
        let url = format!(
            "{}/tv/{}/season/{}/episode/{}?api_key={}&language={}",
            TMDB_BASE_URL, show_id, season, episode, self.api_key, self.language,
        );

        let resp = self.client.get(&url).send().await?;
        if resp.status().is_success() {
            let ep: TmdbEpisode = resp.json().await?;
            Ok(Some(ep))
        } else {
            Ok(None)
        }
    }

    /// Get poster URL for a given path
    pub fn poster_url(path: &str, size: &str) -> String {
        format!("{}/{}{}", TMDB_IMAGE_BASE, size, path)
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}
