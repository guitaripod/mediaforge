use crate::db::models::{AudioTrack, MediaItem, MediaType, PlaybackState, Subtitle};

pub const MEDIA_ITEM_COLUMNS: &str =
    "id, title, sort_title, media_type, year, file_path, file_size,
     duration_secs, video_codec, video_width, video_height, video_bitrate,
     hdr_format, audio_codec, audio_channels, audio_bitrate,
     show_name, season_number, episode_number, episode_title,
     tmdb_id, overview, poster_path, backdrop_path, genres, rating,
     release_date, added_at, updated_at";

pub fn media_item_from_row(row: &rusqlite::Row) -> rusqlite::Result<MediaItem> {
    Ok(MediaItem {
        id: row.get(0)?,
        title: row.get(1)?,
        sort_title: row.get(2)?,
        media_type: row
            .get::<_, String>(3)?
            .parse()
            .unwrap_or(MediaType::Movie),
        year: row.get(4)?,
        file_path: row.get(5)?,
        file_size: row.get(6)?,
        duration_secs: row.get(7)?,
        video_codec: row.get(8)?,
        video_width: row.get(9)?,
        video_height: row.get(10)?,
        video_bitrate: row.get(11)?,
        hdr_format: row.get(12)?,
        audio_codec: row.get(13)?,
        audio_channels: row.get(14)?,
        audio_bitrate: row.get(15)?,
        show_name: row.get(16)?,
        season_number: row.get(17)?,
        episode_number: row.get(18)?,
        episode_title: row.get(19)?,
        tmdb_id: row.get(20)?,
        overview: row.get(21)?,
        poster_path: row.get(22)?,
        backdrop_path: row.get(23)?,
        genres: row.get(24)?,
        rating: row.get(25)?,
        release_date: row.get(26)?,
        added_at: row.get(27)?,
        updated_at: row.get(28)?,
    })
}

pub fn get_subtitles_for_media(
    conn: &rusqlite::Connection,
    media_id: &str,
) -> Result<Vec<Subtitle>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, file_path, stream_index, language, codec, is_forced, is_default, is_external
         FROM subtitles WHERE media_id = ?1",
    )?;

    let subs = stmt
        .query_map([media_id], |row| {
            Ok(Subtitle {
                id: row.get(0)?,
                media_id: row.get(1)?,
                file_path: row.get(2)?,
                stream_index: row.get(3)?,
                language: row.get(4)?,
                codec: row.get(5)?,
                is_forced: row.get::<_, i32>(6)? != 0,
                is_default: row.get::<_, i32>(7)? != 0,
                is_external: row.get::<_, i32>(8)? != 0,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(subs)
}

pub fn get_audio_tracks_for_media(
    conn: &rusqlite::Connection,
    media_id: &str,
) -> Result<Vec<AudioTrack>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, stream_index, codec, language, channels, bitrate, is_default, title
         FROM audio_tracks WHERE media_id = ?1 ORDER BY stream_index",
    )?;

    let tracks = stmt
        .query_map([media_id], |row| {
            Ok(AudioTrack {
                id: row.get(0)?,
                media_id: row.get(1)?,
                stream_index: row.get(2)?,
                codec: row.get(3)?,
                language: row.get(4)?,
                channels: row.get(5)?,
                bitrate: row.get(6)?,
                is_default: row.get::<_, i32>(7)? != 0,
                title: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(tracks)
}

pub fn get_playback_state(
    conn: &rusqlite::Connection,
    media_id: &str,
) -> Result<Option<PlaybackState>, rusqlite::Error> {
    Ok(conn
        .query_row(
            "SELECT media_id, position_secs, is_watched, last_played_at FROM playback_state WHERE media_id = ?1",
            [media_id],
            |row| {
                Ok(PlaybackState {
                    media_id: row.get(0)?,
                    position_secs: row.get(1)?,
                    is_watched: row.get::<_, i32>(2)? != 0,
                    last_played_at: row.get(3)?,
                })
            },
        )
        .ok())
}
