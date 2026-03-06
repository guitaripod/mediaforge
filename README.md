# MediaForge

Personal media server built in Rust. Scans your library, fetches metadata from TMDB, and serves media over HTTP with HLS transcoding and direct streaming.

## Features

- **Library scanning** with automatic filename parsing (scene naming, SxxExx patterns)
- **Adaptive bitrate HLS** with 720p + 360p renditions, keyframe-aligned in a single ffmpeg pass, with real-time progress tracking, seek-to-offset, and cancellation
- **Direct file streaming** with HTTP Range request support
- **TMDB metadata** integration (movie/show/episode lookup, poster proxying)
- **Multi-audio track selection** with per-track language, codec, and channel info
- **Playback tracking** (resume position, watched state, activity history)
- **Watch history** with activity logging (play/pause/complete events)
- **WebSocket** real-time scan progress (no polling needed)
- **Subtitle support** (embedded extraction to WebVTT, external SRT/VTT serving)
- **Thumbnail sprite sheets** — seekbar hover previews with auto-generated VTT + JPEG sprites
- **Smart codec detection** — copies compatible streams, only transcodes when necessary
- **Automatic cache cleanup** — configurable expiry for HLS segments, subtitles, images, and activity logs
- **Docker support** with multi-stage build

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

[cleanup]
interval_secs = 3600
hls_max_age_secs = 86400
subtitle_max_age_secs = 604800
image_max_age_secs = 2592000
activity_retention_days = 90
```

### Run

```sh
mediaforge serve
mediaforge scan
mediaforge config show
mediaforge -c /path/to/config.toml serve
```

### Install as a service (systemd)

```sh
cp contrib/mediaforge.service ~/.config/systemd/user/
# Edit ExecStart path to point to your binary
systemctl --user daemon-reload
systemctl --user enable --now mediaforge
```

Requires lingering for the service to survive logout:
```sh
sudo loginctl enable-linger $USER
```

Manage with:
```sh
systemctl --user status mediaforge
systemctl --user restart mediaforge    # after a rebuild
journalctl --user -u mediaforge -f     # tail logs
```

### Docker

```sh
docker build -t mediaforge .
docker run -d \
  -p 8484:8484 \
  -v /path/to/config:/config \
  -v /path/to/cache:/cache \
  -v /path/to/Movies:/media/movies:ro \
  -v "/path/to/TV Shows:/media/tv:ro" \
  mediaforge
```

Or with docker compose:
```sh
# Edit docker-compose.yml with your media paths
docker compose up -d
```

### Remote access (Tailscale)

MediaForge is designed to run on your home server and be accessed remotely via [Tailscale](https://tailscale.com). Tailscale creates an encrypted mesh VPN between your devices — no port forwarding, no public exposure, no authentication layer needed.

1. Install Tailscale on your server and client devices
2. MediaForge binds to `0.0.0.0:8484` by default, so it's reachable on your Tailscale IP
3. Access from any device on your tailnet: `http://100.x.y.z:8484`

Find your server's Tailscale IP with:
```sh
tailscale ip -4
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
| GET | `/api/library/shows/:id/next` | Next unwatched episode for a show |
| GET | `/api/library/episodes/:id` | Episode detail with subtitles and playback state |
| GET | `/api/library/continue` | In-progress items (resume watching) |
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
| PUT | `/api/playback/:id/state` | Update position (`{ "position_secs": 120.5, "event": "play" }`) |
| POST | `/api/playback/:id/watched` | Mark as watched |
| DELETE | `/api/playback/:id/watched` | Mark as unwatched |
| POST | `/api/playback/shows/:id/watched` | Mark all episodes watched |
| DELETE | `/api/playback/shows/:id/watched` | Mark all episodes unwatched |
| POST | `/api/playback/shows/:id/seasons/:num/watched` | Mark season watched |
| DELETE | `/api/playback/shows/:id/seasons/:num/watched` | Mark season unwatched |
| GET | `/api/playback/history` | Activity log (`?media_id=&limit=50&offset=0`) |

### Streaming

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/stream/:id/info` | Stream info (codec, resolution, transcode needed?) |
| POST | `/api/stream/:id/hls/prepare` | Start HLS generation (`{ "audio_track_id": "...", "start_secs": 120.0 }`) |
| POST | `/api/stream/:id/hls/cancel` | Cancel in-progress transcode |
| GET | `/api/stream/:id/hls/status` | Check HLS readiness (includes progress %) |
| GET | `/api/stream/:id/hls/master.m3u8` | HLS master playlist (adaptive bitrate) |
| GET | `/api/stream/:id/hls/:variant/playlist.m3u8` | HLS variant playlist (720p, 360p, original) |
| GET | `/api/stream/:id/hls/:variant/:segment` | HLS segment |
| GET | `/api/stream/:id/direct` | Direct file stream (supports Range requests) |
| GET | `/api/stream/:id/sprites/sprites.vtt` | Thumbnail sprite map (WebVTT) |
| GET | `/api/stream/:id/sprites/sprites.jpg` | Thumbnail sprite sheet (JPEG) |
| GET | `/api/stream/:id/subtitle/:sub_id` | Subtitle as WebVTT |

### Metadata

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/metadata/scan` | Trigger library scan |
| POST | `/api/metadata/refresh` | Trigger TMDB metadata refresh |
| GET | `/api/metadata/image/*path?size=w500` | Proxy and cache TMDB image (sizes: w92, w154, w185, w342, w500, w780, original) |

### System

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/system/health` | Health check |
| GET | `/api/system/stats` | Library statistics |
| GET | `/api/system/config` | Current config (API key redacted) |
| GET | `/api/system/scan-status` | Scan/metadata fetch progress (idle, scanning, fetching_metadata) |
| WS | `/api/system/ws` | WebSocket for real-time scan progress |

</details>

<details>
<summary><strong>Architecture</strong></summary>

```
Client (any HTTP client)
        |
        | HTTP (JSON + HLS)
        v
MediaForge (Rust/Axum)
  ├── REST API + WebSocket (Axum)
  ├── Media Scanner (walkdir + ffprobe)
  ├── TMDB Metadata Client
  ├── HLS Session Manager (DashMap + Semaphore)
  ├── FFmpeg Wrapper (probe, HLS, remux, subtitles)
  ├── Activity Logger (play/pause/complete tracking)
  └── SQLite Database (WAL mode, r2d2 pool)
```

</details>
