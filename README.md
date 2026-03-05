# MediaForge

Personal media server built in Rust. Scans your library, fetches metadata from TMDB, and serves media over HTTP with HLS transcoding and direct streaming.

## Features

- **Library scanning** with automatic filename parsing (scene naming, SxxExx patterns)
- **HLS streaming** with on-the-fly transcoding for incompatible codecs
- **Direct file streaming** with HTTP Range request support
- **TMDB metadata** integration (movie/show/episode lookup, poster proxying)
- **Playback tracking** (resume position, watched state)
- **Subtitle support** (embedded extraction to WebVTT, external SRT/VTT serving)
- **Smart codec detection** — copies compatible streams, only transcodes when necessary

<details>
<summary><strong>Supported Formats</strong></summary>

| Format | Direct Play? | Action |
|--------|-------------|--------|
| H.264/H.265 in MP4 | Yes | Direct stream |
| H.265 in MKV | No (container) | HLS remux (no re-encode) |
| AV1 | Yes | Direct stream |
| VC-1, XviD, MPEG4 | No | HLS transcode to H.264 |
| AAC, EAC3, AC3 | Yes | Direct |
| TrueHD, DTS | No | Transcode to AAC |
| SRT/VTT subtitles | Yes | Serve as WebVTT |
| PGS/VOBSUB subtitles | No | Returns 422 (bitmap-based) |

</details>

## Setup

### Requirements

- Rust 1.85+ (edition 2024)
- FFmpeg and FFprobe
- TMDB API key (optional, for metadata)

### Build

```sh
cargo build --release
```

### Configure

On first run, a default config is created at `~/.config/mediaforge/config.toml`:

```toml
[server]
host = "0.0.0.0"
port = 8484

[library]
media_dirs = ["/path/to/Movies", "/path/to/TV Shows"]
scan_interval_secs = 300

[tmdb]
api_key = ""
language = "en-US"

[transcoding]
ffmpeg_path = "ffmpeg"
ffprobe_path = "ffprobe"
cache_dir = "/home/you/.cache/mediaforge"
hls_segment_duration = 6
max_concurrent_transcodes = 2
```

### Run

```sh
mediaforge serve
mediaforge scan
mediaforge config show
mediaforge -c /path/to/config.toml serve
```

<details>
<summary><strong>API Reference</strong></summary>

### Library

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/library/movies` | List movies (paginated, sortable) |
| GET | `/api/library/movies/:id` | Movie detail with subtitles and playback state |
| GET | `/api/library/shows` | List TV shows |
| GET | `/api/library/shows/:id` | Show detail with season list |
| GET | `/api/library/shows/:id/seasons/:num` | Episodes in a season |
| GET | `/api/library/recent` | Recently added items |
| GET | `/api/library/search?q=` | Search across all media |

**Query parameters for `/api/library/movies`:**
- `page` — page number (default: 1)
- `per_page` — items per page (default: 50, max: 200)
- `sort` — `title`, `year`, `added`, `rating`

### Playback

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/playback/:id/state` | Get playback position |
| PUT | `/api/playback/:id/state` | Update position (`{ "position_secs": 120.5 }`) |
| POST | `/api/playback/:id/watched` | Mark as watched |
| DELETE | `/api/playback/:id/watched` | Mark as unwatched |

### Streaming

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/stream/:id/info` | Stream info (codec, resolution, transcode needed?) |
| POST | `/api/stream/:id/hls/prepare` | Start HLS generation |
| GET | `/api/stream/:id/hls/status` | Check HLS readiness |
| GET | `/api/stream/:id/hls/playlist.m3u8` | HLS master playlist |
| GET | `/api/stream/:id/hls/:segment` | HLS segment |
| GET | `/api/stream/:id/direct` | Direct file stream (supports Range requests) |
| GET | `/api/stream/:id/subtitle/:sub_id` | Subtitle as WebVTT |

### Metadata

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/metadata/scan` | Trigger library scan |
| POST | `/api/metadata/refresh` | Trigger TMDB metadata refresh |
| GET | `/api/metadata/poster/*path` | Proxy and cache TMDB poster |

### System

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/system/health` | Health check |
| GET | `/api/system/stats` | Library statistics |
| GET | `/api/system/config` | Current config (API key redacted) |

</details>

<details>
<summary><strong>Architecture</strong></summary>

```
Client (any HTTP client)
        |
        | HTTP (JSON + HLS)
        v
MediaForge (Rust/Axum)
  ├── REST API (Axum)
  ├── Media Scanner (walkdir + ffprobe)
  ├── TMDB Metadata Client
  ├── HLS Session Manager (DashMap + Semaphore)
  ├── FFmpeg Wrapper (probe, HLS, remux, subtitles)
  └── SQLite Database (WAL mode, r2d2 pool)
```

</details>
