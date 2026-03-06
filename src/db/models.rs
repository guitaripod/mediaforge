use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    // TV
    pub show_name: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub episode_title: Option<String>,
    // TMDB
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(with = "genres_as_vec")]
    pub genres: Option<String>,
    pub rating: Option<f64>,
    pub release_date: Option<String>,
    // State
    pub added_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub media_id: String,
    pub position_secs: f64,
    pub is_watched: bool,
    pub last_played_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvShow {
    pub id: String,
    pub name: String,
    pub tmdb_id: Option<i64>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(with = "genres_as_vec")]
    pub genres: Option<String>,
    pub rating: Option<f64>,
    pub first_air_date: Option<String>,
    pub added_at: String,
}

/// Summary for library browsing
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvShowSummary {
    pub id: String,
    pub name: String,
    pub poster_path: Option<String>,
    pub rating: Option<f64>,
    pub first_air_date: Option<String>,
    pub season_count: i32,
    pub episode_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeSummary {
    pub id: String,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub episode_title: Option<String>,
    pub duration_secs: Option<f64>,
    pub is_watched: bool,
    pub position_secs: f64,
}

/// Probe result from ffprobe
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityLogEntry {
    pub id: i64,
    pub media_id: String,
    pub event_type: String,
    pub position_secs: f64,
    pub created_at: String,
    pub title: Option<String>,
    pub media_type: Option<String>,
}
