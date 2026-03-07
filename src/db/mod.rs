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
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
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
                poster_blurhash TEXT,
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

            CREATE TABLE IF NOT EXISTS audio_tracks (
                id TEXT PRIMARY KEY,
                media_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
                stream_index INTEGER NOT NULL,
                codec TEXT NOT NULL,
                language TEXT,
                channels INTEGER,
                bitrate INTEGER,
                is_default INTEGER NOT NULL DEFAULT 0,
                title TEXT
            );

            CREATE TABLE IF NOT EXISTS activity_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                media_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
                event_type TEXT NOT NULL CHECK(event_type IN ('play', 'pause', 'complete', 'position_update')),
                position_secs REAL NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS tv_shows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                tmdb_id INTEGER,
                overview TEXT,
                poster_path TEXT,
                backdrop_path TEXT,
                poster_blurhash TEXT,
                genres TEXT,
                rating REAL,
                first_air_date TEXT,
                added_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_media_type ON media_items(media_type);
            CREATE INDEX IF NOT EXISTS idx_show_name ON media_items(show_name);
            CREATE INDEX IF NOT EXISTS idx_file_path ON media_items(file_path);
            CREATE INDEX IF NOT EXISTS idx_tmdb_id ON media_items(tmdb_id);
            CREATE INDEX IF NOT EXISTS idx_added_at ON media_items(added_at);
            CREATE INDEX IF NOT EXISTS idx_sort_title ON media_items(sort_title);
            CREATE INDEX IF NOT EXISTS idx_genres ON media_items(genres);
            CREATE INDEX IF NOT EXISTS idx_title ON media_items(title COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_episode_title ON media_items(episode_title COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_tv_shows_name ON tv_shows(name COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_subtitles_media ON subtitles(media_id);
            CREATE INDEX IF NOT EXISTS idx_audio_tracks_media ON audio_tracks(media_id);
            CREATE INDEX IF NOT EXISTS idx_activity_media ON activity_log(media_id);
            CREATE INDEX IF NOT EXISTS idx_activity_created ON activity_log(created_at);
            CREATE INDEX IF NOT EXISTS idx_playback_continue ON playback_state(is_watched, position_secs)
                WHERE is_watched = 0 AND position_secs > 0;
            ",
        )?;

        for stmt in [
            "ALTER TABLE media_items ADD COLUMN poster_blurhash TEXT",
            "ALTER TABLE tv_shows ADD COLUMN poster_blurhash TEXT",
        ] {
            let _ = conn.execute(stmt, []);
        }

        Ok(())
    }

    pub fn conn(&self) -> r2d2::PooledConnection<SqliteConnectionManager> {
        self.pool.get().expect("failed to get db connection from pool")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        (db, dir)
    }

    #[test]
    fn creates_tables() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"media_items".to_string()));
        assert!(tables.contains(&"subtitles".to_string()));
        assert!(tables.contains(&"playback_state".to_string()));
        assert!(tables.contains(&"tv_shows".to_string()));
        assert!(tables.contains(&"audio_tracks".to_string()));
        assert!(tables.contains(&"activity_log".to_string()));
    }

    #[test]
    fn creates_indexes() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(indexes.contains(&"idx_file_path".to_string()));
        assert!(indexes.contains(&"idx_sort_title".to_string()));
        assert!(indexes.contains(&"idx_title".to_string()));
        assert!(indexes.contains(&"idx_episode_title".to_string()));
        assert!(indexes.contains(&"idx_tv_shows_name".to_string()));
        assert!(indexes.contains(&"idx_playback_continue".to_string()));
        assert!(indexes.contains(&"idx_audio_tracks_media".to_string()));
        assert!(indexes.contains(&"idx_activity_media".to_string()));
        assert!(indexes.contains(&"idx_activity_created".to_string()));
    }

    #[test]
    fn wal_mode_enabled() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn foreign_keys_enabled() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        let fk: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn insert_and_query_media_item() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('test1', 'Test Movie', 'test movie', 'movie', '/tmp/test.mkv', 1000)",
            [],
        ).unwrap();

        let title: String = conn
            .query_row("SELECT title FROM media_items WHERE id = 'test1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "Test Movie");
    }

    #[test]
    fn cascade_delete_subtitles() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO subtitles (id, media_id, codec, is_external) VALUES ('s1', 'm1', 'srt', 1)",
            [],
        ).unwrap();
        conn.execute("DELETE FROM media_items WHERE id = 'm1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM subtitles WHERE media_id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn cascade_delete_playback() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO playback_state (media_id, position_secs) VALUES ('m1', 60.0)",
            [],
        ).unwrap();
        conn.execute("DELETE FROM media_items WHERE id = 'm1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM playback_state WHERE media_id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn playback_upsert() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO playback_state (media_id, position_secs, last_played_at)
             VALUES ('m1', 30.0, datetime('now'))
             ON CONFLICT(media_id) DO UPDATE SET position_secs = 30.0",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO playback_state (media_id, position_secs, last_played_at)
             VALUES ('m1', 90.0, datetime('now'))
             ON CONFLICT(media_id) DO UPDATE SET position_secs = 90.0",
            [],
        ).unwrap();

        let pos: f64 = conn
            .query_row("SELECT position_secs FROM playback_state WHERE media_id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert!((pos - 90.0).abs() < 0.01);
    }

    #[test]
    fn multiple_connections() {
        let (db, _dir) = test_db();
        let c1 = db.conn();
        let c2 = db.conn();
        c1.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();
        let count: i64 = c2
            .query_row("SELECT COUNT(*) FROM media_items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn cascade_delete_audio_tracks() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO audio_tracks (id, media_id, stream_index, codec, language, channels, is_default)
             VALUES ('a1', 'm1', 1, 'aac', 'eng', 2, 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO audio_tracks (id, media_id, stream_index, codec, language, channels, is_default)
             VALUES ('a2', 'm1', 2, 'dts', 'jpn', 6, 0)",
            [],
        ).unwrap();
        conn.execute("DELETE FROM media_items WHERE id = 'm1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM audio_tracks WHERE media_id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn cascade_delete_activity_log() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO activity_log (media_id, event_type, position_secs) VALUES ('m1', 'play', 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO activity_log (media_id, event_type, position_secs) VALUES ('m1', 'complete', 120)",
            [],
        ).unwrap();
        conn.execute("DELETE FROM media_items WHERE id = 'm1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM activity_log WHERE media_id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn activity_log_validates_event_type() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();

        let result = conn.execute(
            "INSERT INTO activity_log (media_id, event_type) VALUES ('m1', 'invalid_event')",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn audio_tracks_multiple_per_media() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO audio_tracks (id, media_id, stream_index, codec, language, channels, is_default)
             VALUES ('a1', 'm1', 1, 'aac', 'eng', 2, 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO audio_tracks (id, media_id, stream_index, codec, language, channels, is_default)
             VALUES ('a2', 'm1', 2, 'ac3', 'jpn', 6, 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO audio_tracks (id, media_id, stream_index, codec, language, channels, is_default, title)
             VALUES ('a3', 'm1', 3, 'dts', 'eng', 8, 0, 'Commentary')",
            [],
        ).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM audio_tracks WHERE media_id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn poster_blurhash_column_exists() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO media_items (id, title, sort_title, media_type, file_path, file_size, poster_blurhash)
             VALUES ('m1', 'Movie', 'movie', 'movie', '/tmp/m.mkv', 100, 'LKO2?U%2Tw=w]~RBVZRi};RPxuwH')",
            [],
        ).unwrap();

        let hash: Option<String> = conn
            .query_row("SELECT poster_blurhash FROM media_items WHERE id = 'm1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(hash.as_deref(), Some("LKO2?U%2Tw=w]~RBVZRi};RPxuwH"));
    }

    #[test]
    fn tv_shows_poster_blurhash_column_exists() {
        let (db, _dir) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO tv_shows (id, name, poster_blurhash) VALUES ('s1', 'Show', 'LEHV6nWB2yk8pyoJadR*.7kCMdnj')",
            [],
        ).unwrap();

        let hash: Option<String> = conn
            .query_row("SELECT poster_blurhash FROM tv_shows WHERE id = 's1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(hash.as_deref(), Some("LEHV6nWB2yk8pyoJadR*.7kCMdnj"));
    }
}
