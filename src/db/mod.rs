use anyhow::Result;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;

pub mod models;

#[derive(Clone)]
pub struct Database {
    pool: Pool<SqliteConnectionManager>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let manager = SqliteConnectionManager::file(path)
            .with_init(|conn| {
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
            });

        let pool = Pool::builder()
            .max_size(8)
            .build(manager)?;

        let db = Self { pool };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.pool.get()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS media_items (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                sort_title TEXT NOT NULL,
                media_type TEXT NOT NULL CHECK(media_type IN ('movie', 'episode')),
                year INTEGER,
                file_path TEXT NOT NULL UNIQUE,
                file_size INTEGER NOT NULL DEFAULT 0,
                duration_secs REAL,
                video_codec TEXT,
                video_width INTEGER,
                video_height INTEGER,
                video_bitrate INTEGER,
                hdr_format TEXT,
                audio_codec TEXT,
                audio_channels INTEGER,
                audio_bitrate INTEGER,
                show_name TEXT,
                season_number INTEGER,
                episode_number INTEGER,
                episode_title TEXT,
                tmdb_id INTEGER,
                overview TEXT,
                poster_path TEXT,
                backdrop_path TEXT,
                genres TEXT,
                rating REAL,
                release_date TEXT,
                added_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS subtitles (
                id TEXT PRIMARY KEY,
                media_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
                file_path TEXT,
                stream_index INTEGER,
                language TEXT,
                codec TEXT,
                is_forced INTEGER NOT NULL DEFAULT 0,
                is_default INTEGER NOT NULL DEFAULT 0,
                is_external INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS playback_state (
                media_id TEXT PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
                position_secs REAL NOT NULL DEFAULT 0,
                is_watched INTEGER NOT NULL DEFAULT 0,
                last_played_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS tv_shows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                tmdb_id INTEGER,
                overview TEXT,
                poster_path TEXT,
                backdrop_path TEXT,
                genres TEXT,
                rating REAL,
                first_air_date TEXT,
                added_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_media_type ON media_items(media_type);
            CREATE INDEX IF NOT EXISTS idx_show_name ON media_items(show_name);
            CREATE INDEX IF NOT EXISTS idx_subtitles_media ON subtitles(media_id);
            ",
        )?;
        Ok(())
    }

    pub fn conn(&self) -> r2d2::PooledConnection<SqliteConnectionManager> {
        self.pool.get().expect("failed to get db connection from pool")
    }
}
