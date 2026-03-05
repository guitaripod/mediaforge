# MediaForge — Build Plan

> Personal Plex alternative: a Rust media server backend designed for an iOS app
> that uses Apple's native `AVPlayerViewController` (smooth scrubber, PiP, AirPlay, etc.)

## Marcus's Instructions (Verbatim, Improved)

> Build the entire thing in `~/Dev/rust/mediaforge` and push the `main` branch to GitHub
> as a new repo. Validate it compiles and uses the latest technology. Do 3 full loops of
> complete implementation and review of all details. Only push to Git once fully satisfied
> and everything mentioned for the backend is built. Media files are on `/mnt/stuff2`.
> Support everything including HLS. Use `gh` CLI for GitHub operations.

---

## Environment

| Tool       | Version / Path                              |
|------------|---------------------------------------------|
| Rust       | `1.92.0-nightly (2025-09-16)`               |
| Cargo      | `1.92.0-nightly`                            |
| FFmpeg     | `n8.0.1` at `/usr/bin/ffmpeg`               |
| FFprobe    | `/usr/bin/ffprobe`                           |
| GitHub CLI | `2.87.1` — authenticated as `marcusziade`   |
| OS         | Arch Linux                                  |

## Media Library (on `/mnt/stuff2`)

### Structure
```
/mnt/stuff2/Movies/          — Movie files (folders or bare files)
/mnt/stuff2/TV Shows/        — TV series (show/season/episode structure)
```

### Formats Found
- **Containers:** MKV (dominant), MP4, AVI
- **Video codecs:** HEVC (H.265), H.264, AV1, VC-1, XviD, x265 10-bit
- **Audio codecs:** EAC3/Atmos, TrueHD/Atmos, DTS-HD, AAC, AC3, DDP5.1, Opus
- **Subtitles:** SubRip (SRT) embedded, external SRT/VTT/ASS
- **Resolutions:** 480p through 4K UHD (3840×2160)
- **HDR:** HDR10, Dolby Vision, HLG, SDR
- **Naming:** Scene naming (`Show.S01E02.Title.Quality.Source.Codec-Group`)

### iOS Compatibility Matrix
| Format | iOS Native? | Action Needed |
|--------|-------------|---------------|
| H.264 in MP4 | ✅ | Direct stream |
| H.265/HEVC in MP4 | ✅ | Direct stream |
| H.265 in MKV | ❌ container | Remux to fMP4 (fast, no re-encode) |
| AV1 | ✅ (iPhone 15+) | Direct or transcode for older devices |
| VC-1 | ❌ | Transcode to H.264/H.265 |
| XviD/DivX | ❌ | Transcode to H.264 |
| AAC | ✅ | Direct |
| EAC3/AC3 | ✅ | Direct |
| TrueHD | ❌ | Transcode to AAC |
| DTS/DTS-HD | ❌ | Transcode to AAC |
| Opus | ❌ | Transcode to AAC |
| SRT/VTT subs | ✅ | Serve as WebVTT sidecar |
| PGS/VOBSUB subs | ❌ | OCR or burn-in (out of scope for MVP) |

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                   iOS App (future)                   │
│  SwiftUI library UI + AVPlayerViewController        │
│  Talks to server via REST API                       │
└──────────────────────┬──────────────────────────────┘
                       │ HTTP (JSON + HLS)
┌──────────────────────▼──────────────────────────────┐
│              MediaForge (Rust Server)                │
│                                                     │
│  ┌──────────┐ ┌──────────┐ ┌───────────────────┐   │
│  │ REST API │ │ Scanner  │ │ TMDB Metadata     │   │
│  │ (Axum)   │ │          │ │ Client            │   │
│  └────┬─────┘ └────┬─────┘ └─────────┬─────────┘   │
│       │            │                  │             │
│  ┌────▼────────────▼──────────────────▼─────────┐   │
│  │              SQLite (WAL mode)                │   │
│  └──────────────────────────────────────────────┘   │
│                                                     │
│  ┌──────────┐ ┌──────────┐ ┌───────────────────┐   │
│  │ FFmpeg   │ │ HLS      │ │ File Watcher      │   │
│  │ Wrapper  │ │ Manager  │ │ (notify)          │   │
│  └──────────┘ └──────────┘ └───────────────────┘   │
└─────────────────────────────────────────────────────┘
```

---

## Module Breakdown

### 1. `src/config/mod.rs` — Configuration ✅ DONE
- TOML-based config at `~/.config/mediaforge/config.toml`
- Server host/port, media dirs, TMDB API key, transcoding settings
- Auto-creates default config on first run

### 2. `src/db/mod.rs` — Database ✅ DONE
- SQLite with WAL mode and foreign keys
- Tables: `media_items`, `subtitles`, `playback_state`, `tv_shows`
- Indexes on media_type, show_name, subtitles

### 3. `src/db/models.rs` — Data Models ✅ DONE
- `MediaItem`, `Subtitle`, `PlaybackState`, `TvShow`
- `MovieSummary`, `TvShowSummary`, `EpisodeSummary` (for list views)
- `ProbeResult`, `SubtitleStream` (from ffprobe)
- `MediaType` enum with Display/FromStr

### 4. `src/scanner/mod.rs` — Media Scanner ✅ DONE
- Recursive directory walking with `walkdir`
- Filename parsing: scene naming, SxxExx patterns, year extraction
- Deduplication (skip already-indexed files)
- External subtitle matching
- TV show auto-creation

### 5. `src/ffmpeg/mod.rs` — FFmpeg Wrapper ✅ DONE
- `probe()` — ffprobe JSON parsing (video/audio/subtitle streams, HDR detection)
- `generate_hls()` — HLS with video copy + audio transcode
- `generate_hls_transcode()` — full video transcode to H.264
- `remux_to_mp4()` — fast MKV→MP4 remux
- `extract_subtitle_vtt()` — subtitle stream → WebVTT
- iOS compatibility checks

### 6. `src/metadata/mod.rs` — TMDB Client ✅ DONE
- Movie search + metadata update
- TV show search + metadata update
- Per-episode metadata (title, overview, air date)
- Rate limiting (250ms between requests)
- Poster/backdrop URL generation

### 7. `src/hls/mod.rs` — HLS Session Manager ✅ DONE
- Session tracking with `DashMap`
- Concurrent transcode limiting via `Semaphore`
- Cache management (reuse existing HLS output)
- Session cleanup (per-item and expired)
- Status tracking (Preparing/Ready/Error)

### 8. `src/api/mod.rs` — Router Setup ✅ DONE
- `AppState` with all shared services
- CORS (allow all for local dev)
- Tracing middleware
- Route group registration

### 9. `src/api/routes.rs` — REST Endpoints ❌ NOT STARTED
This is the main missing piece. Needs:

#### Library Routes (`/api/library/`)
- `GET /api/library/movies` — list all movies (paginated, sortable)
- `GET /api/library/movies/:id` — movie detail with full metadata
- `GET /api/library/shows` — list all TV shows
- `GET /api/library/shows/:id` — show detail with seasons
- `GET /api/library/shows/:id/seasons/:season` — episodes in a season
- `GET /api/library/recent` — recently added items
- `GET /api/library/search?q=` — search across all media

#### Playback Routes (`/api/playback/`)
- `GET /api/playback/:id/state` — get playback position
- `PUT /api/playback/:id/state` — update position / mark watched
- `POST /api/playback/:id/watched` — mark as watched
- `DELETE /api/playback/:id/watched` — mark as unwatched

#### Streaming Routes (`/api/stream/`)
- `GET /api/stream/:id/info` — stream info (codec, resolution, needs transcode?)
- `POST /api/stream/:id/hls/prepare` — kick off HLS generation
- `GET /api/stream/:id/hls/status` — check HLS readiness
- `GET /api/stream/:id/hls/playlist.m3u8` — serve HLS master playlist
- `GET /api/stream/:id/hls/:segment` — serve HLS segment
- `GET /api/stream/:id/direct` — direct file streaming (range requests)
- `GET /api/stream/:id/subtitle/:sub_id` — serve subtitle as WebVTT

#### Metadata Routes (`/api/metadata/`)
- `POST /api/metadata/scan` — trigger library scan
- `POST /api/metadata/refresh` — trigger TMDB metadata refresh
- `GET /api/metadata/poster/:tmdb_path` — proxy TMDB poster (cache locally)

#### System Routes (`/api/system/`)
- `GET /api/system/health` — health check
- `GET /api/system/stats` — library stats (counts, sizes, etc.)
- `GET /api/system/config` — current config (redacted)

### 10. `src/main.rs` — Entry Point ❌ NOT STARTED
- CLI with `clap`: `serve`, `scan`, `config`
- Server startup with graceful shutdown
- Background tasks: periodic scan, HLS cache cleanup
- Config loading and validation

---

## What Still Needs Building

| Component | Status | Priority |
|-----------|--------|----------|
| `src/api/routes.rs` | ❌ Not started | **P0** — server is useless without endpoints |
| `src/main.rs` | ❌ Stub only (`println!`) | **P0** — can't run without it |
| Direct file streaming (range requests) | ❌ | **P0** — for iOS `AVPlayer` direct playback |
| Background scan task | ❌ | **P1** — periodic re-scan |
| File watcher (notify) | ❌ | **P2** — real-time library updates |
| TMDB poster caching/proxy | ❌ | **P1** — avoid iOS app hitting TMDB directly |
| README.md | ❌ | **P1** — project documentation |
| .gitignore | ❌ | **P0** — before any git operations |
| Compilation validation | ❌ | **P0** — must compile clean |
| GitHub repo creation | ❌ | **P0** — final step |

---

## Review Loop Protocol

### Loop 1: Complete Implementation
1. Write `src/api/routes.rs` with all endpoints
2. Write `src/main.rs` with CLI, server startup, background tasks
3. Add direct file streaming with HTTP range request support
4. Add `.gitignore`
5. Verify `cargo build` succeeds with zero errors

### Loop 2: Quality Review
1. Read every file line by line
2. Check for: unused imports, dead code, missing error handling
3. Verify all API routes are consistent and complete
4. Ensure all database queries use proper error handling
5. Check HLS flow end-to-end: prepare → poll status → serve playlist → serve segments
6. Verify direct streaming supports Range headers correctly
7. Fix any issues found
8. `cargo build` again — must be clean
9. `cargo clippy` — fix all warnings

### Loop 3: Polish & Ship
1. Final read-through of all source files
2. Add `README.md` with setup instructions, API reference, architecture
3. Verify config defaults make sense for Marcus's setup
4. Test that default media dirs point to `/mnt/stuff2/Movies` and `/mnt/stuff2/TV Shows`
5. Final `cargo build --release` — must succeed
6. Create GitHub repo, commit, push

---

## Design Decisions

1. **SQLite over Postgres** — single binary, no external deps, plenty fast for personal use
2. **HLS over DASH** — native iOS support via `AVPlayer`, no custom player needed
3. **Axum over Actix** — modern, tokio-native, better ergonomics
4. **Copy when possible** — only transcode when iOS can't play the codec natively
5. **Semaphore-limited transcoding** — don't melt the CPU with concurrent ffmpeg jobs
6. **DashMap for sessions** — lock-free concurrent access to HLS session state
7. **Edition 2024** — latest Rust edition since we're on nightly

---

## File Tree (Target)

```
mediaforge/
├── Cargo.toml
├── .gitignore
├── README.md
├── PLAN.md
└── src/
    ├── main.rs
    ├── config/
    │   └── mod.rs
    ├── db/
    │   ├── mod.rs
    │   └── models.rs
    ├── scanner/
    │   └── mod.rs
    ├── ffmpeg/
    │   └── mod.rs
    ├── metadata/
    │   └── mod.rs
    ├── hls/
    │   └── mod.rs
    └── api/
        ├── mod.rs
        └── routes.rs
```
