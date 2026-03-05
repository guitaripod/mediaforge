mod api;
mod config;
mod db;
mod ffmpeg;
mod hls;
mod metadata;
mod scanner;

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use api::AppState;
use config::Config;
use db::Database;
use ffmpeg::FFmpeg;
use hls::HlsManager;
use metadata::TmdbClient;
use scanner::Scanner;

#[derive(Parser)]
#[command(name = "mediaforge", version, about = "Personal media server with HLS streaming")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, help = "Path to config file")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    Serve,
    Scan,
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    Show,
    Path,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let config_path = cli.config.unwrap_or_else(Config::config_path);
    let config = Config::load(&config_path)?;

    match cli.command.unwrap_or(Commands::Serve) {
        Commands::Serve => run_server(config).await,
        Commands::Scan => run_scan(config).await,
        Commands::Config { action } => {
            match action {
                ConfigAction::Show => {
                    println!("{}", toml::to_string_pretty(&config)?);
                }
                ConfigAction::Path => {
                    println!("{}", config_path.display());
                }
            }
            Ok(())
        }
    }
}

async fn run_server(config: Config) -> anyhow::Result<()> {
    let db_path = config.transcoding.cache_dir.join("mediaforge.db");
    let db = Database::open(&db_path)?;

    let ffmpeg = FFmpeg::new(
        config.transcoding.ffmpeg_path.clone(),
        config.transcoding.ffprobe_path.clone(),
    );

    let hls = HlsManager::new(
        ffmpeg.clone(),
        config.transcoding.cache_dir.clone(),
        config.transcoding.hls_segment_duration,
        config.transcoding.max_concurrent_transcodes,
    );

    let tmdb = TmdbClient::new(
        config.tmdb.api_key.clone(),
        config.tmdb.language.clone(),
    );

    let state = AppState {
        db: db.clone(),
        ffmpeg: ffmpeg.clone(),
        hls: hls.clone(),
        tmdb: tmdb.clone(),
        config: config.clone(),
    };

    let scan_db = db.clone();
    let scan_ffmpeg = ffmpeg.clone();
    let scan_dirs = config.library.media_dirs.clone();
    let scan_interval = config.library.scan_interval_secs;
    tokio::spawn(async move {
        if !scan_dirs.is_empty() {
            let scanner = Scanner::new(scan_db.clone(), scan_ffmpeg.clone());
            info!("Running initial library scan...");
            if let Err(e) = scanner.scan_directories(&scan_dirs).await {
                error!("Initial scan failed: {}", e);
            }

            let mut interval = tokio::time::interval(Duration::from_secs(scan_interval));
            interval.tick().await;
            loop {
                interval.tick().await;
                info!("Running periodic library scan...");
                let scanner = Scanner::new(scan_db.clone(), scan_ffmpeg.clone());
                if let Err(e) = scanner.scan_directories(&scan_dirs).await {
                    error!("Periodic scan failed: {}", e);
                }
            }
        }
    });

    let cleanup_hls = hls.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            if let Err(e) = cleanup_hls.cleanup_expired(Duration::from_secs(86400)) {
                error!("HLS cleanup failed: {}", e);
            }
        }
    });

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("MediaForge listening on http://{}", addr);

    let router = api::create_router(state);

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Server shut down gracefully");
    Ok(())
}

async fn run_scan(config: Config) -> anyhow::Result<()> {
    let db_path = config.transcoding.cache_dir.join("mediaforge.db");
    let db = Database::open(&db_path)?;

    let ffmpeg = FFmpeg::new(
        config.transcoding.ffmpeg_path.clone(),
        config.transcoding.ffprobe_path.clone(),
    );

    let scanner = Scanner::new(db.clone(), ffmpeg);
    info!("Scanning media directories...");
    scanner.scan_directories(&config.library.media_dirs).await?;

    let tmdb = TmdbClient::new(config.tmdb.api_key.clone(), config.tmdb.language.clone());
    if tmdb.has_key() {
        info!("Migrating genres...");
        tmdb.migrate_numeric_genres(&db).await?;
        info!("Fetching TMDB metadata...");
        tmdb.update_movie_metadata(&db).await?;
        tmdb.update_tv_metadata(&db).await?;
    }

    info!("Scan complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received");
}
