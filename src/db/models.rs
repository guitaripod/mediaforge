use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod genres_as_vec {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(s) if !s.is_empty() => {
                let vec: Vec<&str> = s.split(',').map(|g| g.trim()).collect();
                serializer.serialize_some(&vec)
            }
            _ => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        Ok(opt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MediaItem {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing)]
    pub sort_title: String,
    pub media_type: MediaType,
    pub year: Option<i32>,
    #[serde(skip_serializing)]
    pub file_path: String,
    pub file_size: i64,
    pub duration_secs: Option<f64>,
    pub video_codec: Option<String>,
    pub video_width: Option<i32>,
    pub video_height: Option<i32>,
    #[serde(skip_serializing)]
    pub video_bitrate: Option<i64>,
    pub hdr_format: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<i32>,
    #[serde(skip_serializing)]
    pub audio_bitrate: Option<i64>,
    pub show_name: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub episode_title: Option<String>,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(with = "genres_as_vec")]
    #[schema(value_type = Option<Vec<String>>)]
    pub genres: Option<String>,
    pub rating: Option<f64>,
    pub release_date: Option<String>,
    pub added_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Movie,
    Episode,
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaType::Movie => write!(f, "movie"),
            MediaType::Episode => write!(f, "episode"),
        }
    }
}

impl std::str::FromStr for MediaType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "movie" => Ok(MediaType::Movie),
            "episode" => Ok(MediaType::Episode),
            _ => Err(anyhow::anyhow!("Unknown media type: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Subtitle {
    pub id: String,
    pub media_id: String,
    #[serde(skip_serializing)]
    pub file_path: Option<String>,
    pub stream_index: Option<i32>,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub is_forced: bool,
    pub is_default: bool,
    pub is_external: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaybackState {
    pub media_id: String,
    pub position_secs: f64,
    pub is_watched: bool,
    pub last_played_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TvShow {
    pub id: String,
    pub name: String,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(with = "genres_as_vec")]
    #[schema(value_type = Option<Vec<String>>)]
    pub genres: Option<String>,
    pub rating: Option<f64>,
    pub first_air_date: Option<String>,
    pub added_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MediaSummary {
    pub id: String,
    pub title: String,
    pub media_type: String,
    pub year: Option<i32>,
    pub poster_path: Option<String>,
    pub rating: Option<f64>,
    pub duration_secs: Option<f64>,
    pub video_width: Option<i32>,
    pub video_height: Option<i32>,
    pub hdr_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TvShowSummary {
    pub id: String,
    pub name: String,
    pub poster_path: Option<String>,
    pub rating: Option<f64>,
    pub first_air_date: Option<String>,
    pub season_count: i32,
    pub episode_count: i32,
    pub watched_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EpisodeSummary {
    pub id: String,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub episode_title: Option<String>,
    pub duration_secs: Option<f64>,
    pub is_watched: bool,
    pub position_secs: f64,
}

pub struct ProbeResult {
    pub duration_secs: Option<f64>,
    pub video_codec: Option<String>,
    pub video_width: Option<i32>,
    pub video_height: Option<i32>,
    pub video_bitrate: Option<i64>,
    pub hdr_format: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<i32>,
    pub audio_bitrate: Option<i64>,
    pub subtitle_streams: Vec<SubtitleStream>,
    pub audio_streams: Vec<AudioStream>,
}

#[derive(Debug, Clone)]
pub struct SubtitleStream {
    pub index: i32,
    pub codec: String,
    pub language: Option<String>,
    pub is_forced: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub index: i32,
    pub codec: String,
    pub language: Option<String>,
    pub channels: Option<i32>,
    pub bitrate: Option<i64>,
    pub is_default: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AudioTrack {
    pub id: String,
    pub media_id: String,
    pub stream_index: i32,
    pub codec: String,
    pub language: Option<String>,
    pub channels: Option<i32>,
    pub bitrate: Option<i64>,
    pub is_default: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityLogEntry {
    pub id: i64,
    pub media_id: String,
    pub event_type: String,
    pub position_secs: f64,
    pub created_at: String,
    pub title: Option<String>,
    pub media_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_parse_movie() {
        assert_eq!("movie".parse::<MediaType>().unwrap(), MediaType::Movie);
    }

    #[test]
    fn media_type_parse_episode() {
        assert_eq!("episode".parse::<MediaType>().unwrap(), MediaType::Episode);
    }

    #[test]
    fn media_type_parse_invalid() {
        assert!("show".parse::<MediaType>().is_err());
        assert!("".parse::<MediaType>().is_err());
        assert!("Movie".parse::<MediaType>().is_err());
    }

    #[test]
    fn media_type_display() {
        assert_eq!(MediaType::Movie.to_string(), "movie");
        assert_eq!(MediaType::Episode.to_string(), "episode");
    }

    #[test]
    fn genres_as_vec_serializes_csv_to_array() {
        let item = MediaItem {
            id: "t".into(), title: "T".into(), sort_title: "t".into(),
            media_type: MediaType::Movie, year: None, file_path: "/x".into(),
            file_size: 0, duration_secs: None, video_codec: None, video_width: None,
            video_height: None, video_bitrate: None, hdr_format: None, audio_codec: None,
            audio_channels: None, audio_bitrate: None, show_name: None, season_number: None,
            episode_number: None, episode_title: None, tmdb_id: None, overview: None,
            poster_path: None, backdrop_path: None,
            genres: Some("Action, Comedy, Drama".into()),
            rating: None, release_date: None,
            added_at: "2024-01-01".into(), updated_at: "2024-01-01".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        let genres = json["genres"].as_array().unwrap();
        assert_eq!(genres.len(), 3);
        assert_eq!(genres[0], "Action");
        assert_eq!(genres[1], "Comedy");
        assert_eq!(genres[2], "Drama");
    }

    #[test]
    fn genres_as_vec_serializes_none_as_null() {
        let item = MediaItem {
            id: "t".into(), title: "T".into(), sort_title: "t".into(),
            media_type: MediaType::Movie, year: None, file_path: "/x".into(),
            file_size: 0, duration_secs: None, video_codec: None, video_width: None,
            video_height: None, video_bitrate: None, hdr_format: None, audio_codec: None,
            audio_channels: None, audio_bitrate: None, show_name: None, season_number: None,
            episode_number: None, episode_title: None, tmdb_id: None, overview: None,
            poster_path: None, backdrop_path: None, genres: None,
            rating: None, release_date: None,
            added_at: "2024-01-01".into(), updated_at: "2024-01-01".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(json["genres"].is_null());
    }

    #[test]
    fn genres_as_vec_serializes_empty_string_as_null() {
        let item = MediaItem {
            id: "t".into(), title: "T".into(), sort_title: "t".into(),
            media_type: MediaType::Movie, year: None, file_path: "/x".into(),
            file_size: 0, duration_secs: None, video_codec: None, video_width: None,
            video_height: None, video_bitrate: None, hdr_format: None, audio_codec: None,
            audio_channels: None, audio_bitrate: None, show_name: None, season_number: None,
            episode_number: None, episode_title: None, tmdb_id: None, overview: None,
            poster_path: None, backdrop_path: None, genres: Some("".into()),
            rating: None, release_date: None,
            added_at: "2024-01-01".into(), updated_at: "2024-01-01".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(json["genres"].is_null());
    }

    #[test]
    fn media_item_skips_internal_fields() {
        let item = MediaItem {
            id: "t".into(), title: "T".into(), sort_title: "internal".into(),
            media_type: MediaType::Movie, year: None,
            file_path: "/secret/path.mkv".into(),
            file_size: 100, duration_secs: None, video_codec: None, video_width: None,
            video_height: None, video_bitrate: Some(5000), hdr_format: None,
            audio_codec: None, audio_channels: None, audio_bitrate: Some(320),
            show_name: None, season_number: None, episode_number: None, episode_title: None,
            tmdb_id: None, overview: None, poster_path: None, backdrop_path: None,
            genres: None, rating: None, release_date: None,
            added_at: "2024-01-01".into(), updated_at: "2024-01-01".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(!json.as_object().unwrap().contains_key("sort_title"));
        assert!(!json.as_object().unwrap().contains_key("file_path"));
        assert!(!json.as_object().unwrap().contains_key("video_bitrate"));
        assert!(!json.as_object().unwrap().contains_key("audio_bitrate"));
    }

    #[test]
    fn playback_state_serialization() {
        let ps = PlaybackState {
            media_id: "m1".into(),
            position_secs: 42.5,
            is_watched: false,
            last_played_at: None,
        };
        let json = serde_json::to_value(&ps).unwrap();
        assert_eq!(json["media_id"], "m1");
        assert_eq!(json["position_secs"], 42.5);
        assert_eq!(json["is_watched"], false);
        assert!(json["last_played_at"].is_null());
    }

    #[test]
    fn subtitle_skips_file_path() {
        let sub = Subtitle {
            id: "s1".into(), media_id: "m1".into(),
            file_path: Some("/secret/path.srt".into()),
            stream_index: Some(2), language: Some("eng".into()),
            codec: Some("subrip".into()), is_forced: false, is_default: true,
            is_external: true,
        };
        let json = serde_json::to_value(&sub).unwrap();
        assert!(!json.as_object().unwrap().contains_key("file_path"));
        assert_eq!(json["language"], "eng");
        assert_eq!(json["is_default"], true);
    }
}
