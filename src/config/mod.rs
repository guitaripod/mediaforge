use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub library: LibraryConfig,
    pub tmdb: TmdbConfig,
    pub transcoding: TranscodingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryConfig {
    pub media_dirs: Vec<PathBuf>,
    pub scan_interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbConfig {
    pub api_key: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodingConfig {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub cache_dir: PathBuf,
    pub hls_segment_duration: u32,
    pub max_concurrent_transcodes: usize,
}

impl Default for Config {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("mediaforge");

        Self {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8484,
            },
            library: LibraryConfig {
                media_dirs: vec![
                    PathBuf::from("/mnt/stuff2/Movies"),
                    PathBuf::from("/mnt/stuff2/TV Shows"),
                ],
                scan_interval_secs: 300,
            },
            tmdb: TmdbConfig {
                api_key: String::new(),
                language: "en-US".to_string(),
            },
            transcoding: TranscodingConfig {
                ffmpeg_path: PathBuf::from("ffmpeg"),
                ffprobe_path: PathBuf::from("ffprobe"),
                cache_dir,
                hls_segment_duration: 6,
                max_concurrent_transcodes: 2,
            },
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            let config = Config::default();
            config.save(path)?;
            Ok(config)
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mediaforge")
            .join("config.toml")
    }
}
