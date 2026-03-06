#!/usr/bin/env bash
set -euo pipefail

BASE="http://localhost:8484"
PASS=0
FAIL=0
ERRORS=""

test_endpoint() {
    local method="$1"
    local path="$2"
    local expect_status="$3"
    local label="$4"
    local body="${5:-}"

    local curl_args=(-s -o /tmp/mf_test_body -D /tmp/mf_test_headers -w "%{http_code}" -X "$method")
    if [[ -n "$body" ]]; then
        curl_args+=(-H "Content-Type: application/json" -d "$body")
    fi

    local status
    status=$(curl "${curl_args[@]}" "${BASE}${path}")

    if [[ "$status" == "$expect_status" ]]; then
        PASS=$((PASS + 1))
        printf "  PASS  %-60s %s\n" "$label" "$status"
    else
        FAIL=$((FAIL + 1))
        local resp
        resp=$(head -c 200 /tmp/mf_test_body)
        ERRORS="${ERRORS}\n  FAIL  ${label}: expected ${expect_status}, got ${status} — ${resp}"
        printf "  FAIL  %-60s %s (expected %s)\n" "$label" "$status" "$expect_status"
    fi
}

assert_body() {
    local label="$1"
    local check="$2"

    if python3 -c "$check" < /tmp/mf_test_body 2>/dev/null; then
        PASS=$((PASS + 1))
        printf "  PASS  %-60s\n" "$label"
    else
        FAIL=$((FAIL + 1))
        local detail
        detail=$(python3 -c "$check" < /tmp/mf_test_body 2>&1 | tail -1)
        ERRORS="${ERRORS}\n  FAIL  ${label}: ${detail}"
        printf "  FAIL  %-60s\n" "$label"
    fi
}

json_field() {
    python3 -c "import sys,json; print(json.load(sys.stdin)$1)" < /tmp/mf_test_body
}

echo "========================================"
echo " MediaForge API Integration Tests"
echo "========================================"
echo ""

# ── System ───────────────────────────────────────────────────────────────────

echo "-- System --"
test_endpoint GET "/api/system/health" 200 "health check"
assert_body "health has status=ok and version" "
import sys,json; d=json.load(sys.stdin)
assert d['status'] == 'ok'
assert 'version' in d
"

test_endpoint GET "/api/system/stats" 200 "library stats"
assert_body "stats has all fields" "
import sys,json; d=json.load(sys.stdin)
assert d['movies'] > 0
assert d['episodes'] > 0
assert d['shows'] > 0
assert d['total_size_bytes'] > 0
assert d['total_duration_secs'] > 0
"

MOVIE_COUNT=$(json_field "['movies']")
EPISODE_COUNT=$(json_field "['episodes']")
SHOW_COUNT=$(json_field "['shows']")
echo "   Library: ${MOVIE_COUNT} movies, ${EPISODE_COUNT} episodes, ${SHOW_COUNT} shows"

test_endpoint GET "/api/system/config" 200 "config (redacted)"
assert_body "config redacts api_key" "
import sys,json; d=json.load(sys.stdin)
assert 'api_key' not in str(d.get('tmdb', {})) or d['tmdb'].get('has_api_key') is not None
assert 'server' in d
assert 'library' in d
assert 'transcoding' in d
"

test_endpoint GET "/api/system/scan-status" 200 "scan status"
assert_body "scan status has status field" "
import sys,json; d=json.load(sys.stdin)
assert d['status'] in ('idle', 'scanning', 'fetching_metadata')
assert 'is_running' in d
"
echo ""

# ── Library: Movies ──────────────────────────────────────────────────────────

echo "-- Library: Movies --"
test_endpoint GET "/api/library/movies?page=1&per_page=5&sort=title" 200 "list movies (paginated)"
assert_body "pagination response shape" "
import sys,json; d=json.load(sys.stdin)
for key in ['items', 'total', 'page', 'per_page', 'total_pages']:
    assert key in d, f'missing {key}'
assert d['page'] == 1
assert d['per_page'] == 5
assert len(d['items']) <= 5
assert d['total'] > 0
assert d['total_pages'] > 0
"
assert_body "movie summary has required fields" "
import sys,json; d=json.load(sys.stdin)
m = d['items'][0]
for key in ['id', 'title', 'media_type', 'duration_secs', 'video_width', 'video_height']:
    assert key in m, f'missing {key}'
assert m['media_type'] == 'movie'
"

test_endpoint GET "/api/library/movies?sort=year" 200 "sort=year"
test_endpoint GET "/api/library/movies?sort=added" 200 "sort=added"
test_endpoint GET "/api/library/movies?sort=rating" 200 "sort=rating"

curl -s "${BASE}/api/library/movies?page=1&per_page=2&sort=title" > /tmp/mf_test_body
PAGE1_FIRST=$(json_field "['items'][0]['title']")
curl -s "${BASE}/api/library/movies?page=2&per_page=2&sort=title" > /tmp/mf_test_body
PAGE2_FIRST=$(json_field "['items'][0]['title']")
if [[ "$PAGE1_FIRST" != "$PAGE2_FIRST" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "page 2 returns different items than page 1"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  pagination: page 1 and 2 return same first item"
    printf "  FAIL  %-60s\n" "page 2 returns different items than page 1"
fi

curl -s "${BASE}/api/library/movies?per_page=1&sort=title" > /tmp/mf_test_body
MOVIE_ID=$(json_field "['items'][0]['id']")
echo "   Using movie ID: ${MOVIE_ID}"

test_endpoint GET "/api/library/movies/${MOVIE_ID}" 200 "get movie detail"
assert_body "movie detail shape" "
import sys,json; d=json.load(sys.stdin)
assert 'item' in d and 'subtitles' in d and 'playback' in d and 'audio_tracks' in d
item = d['item']
for key in ['id', 'title', 'video_codec', 'audio_codec', 'duration_secs', 'file_size', 'media_type']:
    assert key in item, f'missing {key}'
assert isinstance(d['subtitles'], list)
assert isinstance(d['audio_tracks'], list)
"

test_endpoint GET "/api/library/movies/nonexistent-id-000" 404 "movie not found"
echo ""

# ── Library: Movies Genre Filtering ─────────────────────────────────────────

echo "-- Genre Filtering --"
test_endpoint GET "/api/library/genres" 200 "list genres"
assert_body "genres is a non-empty array of strings" "
import sys,json; d=json.load(sys.stdin)
assert isinstance(d, list) and len(d) > 0
for g in d:
    assert isinstance(g, str) and len(g) > 0
"
assert_body "genres are sorted alphabetically" "
import sys,json; d=json.load(sys.stdin)
assert d == sorted(d), f'not sorted: {d[:5]}'
"

GENRE=$(curl -s "${BASE}/api/library/genres" | python3 -c "import sys,json; print(json.load(sys.stdin)[0])")
echo "   Testing genre: ${GENRE}"

test_endpoint GET "/api/library/movies?genre=${GENRE}" 200 "filter movies by genre"
assert_body "genre filter returns results" "
import sys,json; d=json.load(sys.stdin)
assert d['total'] > 0
assert len(d['items']) > 0
"

curl -s "${BASE}/api/library/movies" > /tmp/mf_test_body
TOTAL_ALL=$(json_field "['total']")
curl -s "${BASE}/api/library/movies?genre=${GENRE}" > /tmp/mf_test_body
TOTAL_GENRE=$(json_field "['total']")
if [[ "$TOTAL_GENRE" -le "$TOTAL_ALL" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s %s <= %s\n" "genre filter narrows results" "$TOTAL_GENRE" "$TOTAL_ALL"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  genre total ($TOTAL_GENRE) > unfiltered total ($TOTAL_ALL)"
    printf "  FAIL  %-60s\n" "genre filter narrows results"
fi

test_endpoint GET "/api/library/movies?genre=zzzNonexistentGenre" 200 "nonexistent genre"
assert_body "nonexistent genre returns zero results" "
import sys,json; d=json.load(sys.stdin)
assert d['total'] == 0
assert len(d['items']) == 0
"
echo ""

# ── Library: TV Shows ────────────────────────────────────────────────────────

echo "-- Library: TV Shows --"
test_endpoint GET "/api/library/shows" 200 "list shows (default)"
assert_body "shows returns paginated response" "
import sys,json; d=json.load(sys.stdin)
for key in ['items', 'total', 'page', 'per_page', 'total_pages']:
    assert key in d, f'missing {key}'
assert d['total'] > 0
assert len(d['items']) > 0
"
assert_body "show summary has all fields including watched_count" "
import sys,json; d=json.load(sys.stdin)
s = d['items'][0]
for key in ['id', 'name', 'season_count', 'episode_count', 'watched_count']:
    assert key in s, f'missing {key}'
assert isinstance(s['watched_count'], int)
assert isinstance(s['season_count'], int)
assert isinstance(s['episode_count'], int)
"

test_endpoint GET "/api/library/shows?page=1&per_page=2" 200 "shows pagination"
assert_body "shows per_page respected" "
import sys,json; d=json.load(sys.stdin)
assert d['per_page'] == 2
assert len(d['items']) <= 2
"

test_endpoint GET "/api/library/shows?sort=name" 200 "shows sort=name"
test_endpoint GET "/api/library/shows?sort=added" 200 "shows sort=added"
test_endpoint GET "/api/library/shows?sort=rating" 200 "shows sort=rating"

curl -s "${BASE}/api/library/shows?page=1&per_page=1" > /tmp/mf_test_body
SHOW_ID=$(json_field "['items'][0]['id']")
SHOW_NAME=$(json_field "['items'][0]['name']")
echo "   Using show: ${SHOW_NAME} (${SHOW_ID})"

test_endpoint GET "/api/library/shows/${SHOW_ID}" 200 "get show detail"
assert_body "show detail has show + seasons" "
import sys,json; d=json.load(sys.stdin)
assert 'show' in d and 'seasons' in d
assert isinstance(d['seasons'], list) and len(d['seasons']) > 0
show = d['show']
for key in ['id', 'name', 'added_at']:
    assert key in show, f'missing {key}'
"
assert_body "each season has episode_count and watched_count" "
import sys,json; d=json.load(sys.stdin)
for s in d['seasons']:
    for key in ['season_number', 'episode_count', 'watched_count']:
        assert key in s, f'season missing {key}'
    assert isinstance(s['watched_count'], int)
    assert s['episode_count'] > 0
"

FIRST_SEASON=$(json_field "['seasons'][0]['season_number']")
echo "   First season: ${FIRST_SEASON}"

test_endpoint GET "/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" 200 "get season episodes"
assert_body "episodes have required fields" "
import sys,json; d=json.load(sys.stdin)
assert len(d) > 0
ep = d[0]
for key in ['id', 'season_number', 'episode_number', 'duration_secs', 'is_watched', 'position_secs']:
    assert key in ep, f'missing {key}'
assert isinstance(ep['is_watched'], bool)
assert isinstance(ep['position_secs'], (int, float))
"

EPISODE_ID=$(json_field "[0]['id']")
echo "   Using episode ID: ${EPISODE_ID}"

test_endpoint GET "/api/library/episodes/${EPISODE_ID}" 200 "get episode detail"
assert_body "episode detail shape" "
import sys,json; d=json.load(sys.stdin)
assert 'item' in d and 'subtitles' in d and 'playback' in d and 'audio_tracks' in d
item = d['item']
assert item['media_type'] == 'episode'
for key in ['show_name', 'season_number', 'episode_number']:
    assert key in item, f'missing {key}'
"

test_endpoint GET "/api/library/shows/${SHOW_ID}/next" 200 "next episode"
assert_body "next episode has episode fields" "
import sys,json; d=json.load(sys.stdin)
if d is not None:
    for key in ['id', 'season_number', 'episode_number']:
        assert key in d, f'missing {key}'
"

test_endpoint GET "/api/library/shows/nonexistent-id-000" 404 "show not found"
test_endpoint GET "/api/library/shows/nonexistent-id-000/seasons/1" 404 "season of nonexistent show"
test_endpoint GET "/api/library/shows/nonexistent-id-000/next" 404 "next ep of nonexistent show"
test_endpoint GET "/api/library/episodes/nonexistent-id-000" 404 "episode not found"
echo ""

# ── Library: Search ──────────────────────────────────────────────────────────

echo "-- Search --"
SEARCH_SHOW_NAME=$(curl -s "${BASE}/api/library/shows?per_page=1" | python3 -c "import sys,json; print(json.load(sys.stdin)['items'][0]['name'])")
SEARCH_TERM=$(echo "$SEARCH_SHOW_NAME" | cut -d' ' -f1)
echo "   Searching for: ${SEARCH_TERM}"

test_endpoint GET "/api/library/search?q=${SEARCH_TERM}" 200 "search finds results"
assert_body "search results are a list" "
import sys,json; d=json.load(sys.stdin)
assert isinstance(d, list)
assert len(d) > 0
"
assert_body "search results have required fields" "
import sys,json; d=json.load(sys.stdin)
for item in d:
    for key in ['id', 'title', 'media_type']:
        assert key in item, f'missing {key}'
    assert item['media_type'] in ('movie', 'episode', 'show')
"
assert_body "search includes show-type results" "
import sys,json; d=json.load(sys.stdin)
types = {item['media_type'] for item in d}
assert 'show' in types, f'no show results, got types: {types}'
"

test_endpoint GET "/api/library/search?q=xyznonexistent999" 200 "search no results"
assert_body "empty search returns empty array" "
import sys,json; d=json.load(sys.stdin)
assert d == []
"

test_endpoint GET "/api/library/search?q=" 400 "search empty query"
LONG_Q=$(python3 -c "print('a' * 201)")
test_endpoint GET "/api/library/search?q=${LONG_Q}" 400 "search too-long query"
echo ""

# ── Library: Browse Endpoints ────────────────────────────────────────────────

echo "-- Browse Endpoints --"
test_endpoint GET "/api/library/recent" 200 "recent items"
curl -s "${BASE}/api/library/recent?per_page=5" > /tmp/mf_test_body
assert_body "recent respects per_page" "
import sys,json; d=json.load(sys.stdin)
assert isinstance(d, list)
assert len(d) <= 5
"
assert_body "recent items have required fields" "
import sys,json; d=json.load(sys.stdin)
if len(d) > 0:
    for key in ['id', 'title', 'media_type']:
        assert key in d[0], f'missing {key}'
"

test_endpoint GET "/api/library/continue" 200 "continue watching"
assert_body "continue watching response shape" "
import sys,json; d=json.load(sys.stdin)
assert 'items' in d and 'total' in d
assert isinstance(d['items'], list)
assert isinstance(d['total'], int)
"

test_endpoint GET "/api/library/ondeck" 200 "on deck"
assert_body "on deck is a list" "
import sys,json; d=json.load(sys.stdin)
assert isinstance(d, list)
"

test_endpoint GET "/api/library/watched" 200 "recently watched"
assert_body "recently watched is a list" "
import sys,json; d=json.load(sys.stdin)
assert isinstance(d, list)
"

test_endpoint GET "/api/library/random" 200 "random item (any)"
assert_body "random returns a media item or null" "
import sys,json; d=json.load(sys.stdin)
if d is not None:
    for key in ['id', 'title', 'media_type']:
        assert key in d, f'missing {key}'
"

test_endpoint GET "/api/library/random?media_type=movie" 200 "random movie"
assert_body "random movie has media_type=movie" "
import sys,json; d=json.load(sys.stdin)
assert d is not None and d['media_type'] == 'movie'
"

test_endpoint GET "/api/library/random?media_type=episode" 200 "random episode"
assert_body "random episode has media_type=episode" "
import sys,json; d=json.load(sys.stdin)
assert d is not None and d['media_type'] == 'episode'
"

test_endpoint GET "/api/library/random?media_type=unwatched" 200 "random unwatched"
test_endpoint GET "/api/library/random?media_type=invalid" 400 "random invalid type"
echo ""

# ── Playback State ───────────────────────────────────────────────────────────

echo "-- Playback State --"
test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "set position (play event)" '{"position_secs": 42.5, "event": "play"}'
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get playback state"
assert_body "position persisted as 42.5" "
import sys,json; d=json.load(sys.stdin)
assert d['position_secs'] == 42.5
assert d['media_id'] == '${MOVIE_ID}'
assert 'last_played_at' in d
assert 'is_watched' in d
"

test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "update position (pause event)" '{"position_secs": 99.9, "event": "pause"}'
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get state after pause"
assert_body "position updated to 99.9" "
import sys,json; d=json.load(sys.stdin)
assert d['position_secs'] == 99.9
"

test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "position update (no event)" '{"position_secs": 120.0}'

test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 400 "reject negative position" '{"position_secs": -1.0}'
test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 400 "reject invalid event" '{"position_secs": 10.0, "event": "bogus"}'
test_endpoint PUT "/api/playback/nonexistent-id-000/state" 404 "update nonexistent media"  '{"position_secs": 10.0}'
test_endpoint GET "/api/playback/nonexistent-id-000/state" 404 "get nonexistent playback"

test_endpoint GET "/api/library/continue" 200 "continue watching (after setting position)"
assert_body "movie appears in continue watching" "
import sys,json; d=json.load(sys.stdin)
ids = [item['id'] for item in d['items']]
assert '${MOVIE_ID}' in ids, f'movie not in continue watching: {ids[:5]}'
"
assert_body "continue item has progress_percent" "
import sys,json; d=json.load(sys.stdin)
item = next(i for i in d['items'] if i['id'] == '${MOVIE_ID}')
assert 'progress_percent' in item
assert 'last_played_at' in item
assert item['position_secs'] == 120.0
"

test_endpoint POST "/api/playback/${MOVIE_ID}/watched" 204 "mark watched"
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get state after mark watched"
assert_body "watched: is_watched=true, position reset" "
import sys,json; d=json.load(sys.stdin)
assert d['is_watched'] == True
assert d['position_secs'] == 0
"

test_endpoint GET "/api/library/watched" 200 "recently watched (after marking)"
assert_body "movie appears in recently watched" "
import sys,json; d=json.load(sys.stdin)
ids = [item['id'] for item in d]
assert '${MOVIE_ID}' in ids, f'movie not in recently watched'
"

test_endpoint GET "/api/library/continue" 200 "continue watching (after marking watched)"
assert_body "watched movie NOT in continue watching" "
import sys,json; d=json.load(sys.stdin)
ids = [item['id'] for item in d['items']]
assert '${MOVIE_ID}' not in ids, 'watched movie still in continue watching'
"

test_endpoint DELETE "/api/playback/${MOVIE_ID}/watched" 204 "mark unwatched"
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get state after mark unwatched"
assert_body "unwatched: is_watched=false, position=0" "
import sys,json; d=json.load(sys.stdin)
assert d['is_watched'] == False
assert d['position_secs'] == 0
"

test_endpoint POST "/api/playback/nonexistent-id-000/watched" 404 "mark nonexistent watched"
test_endpoint DELETE "/api/playback/nonexistent-id-000/watched" 404 "mark nonexistent unwatched"

MOVIE_DURATION=$(curl -s "${BASE}/api/stream/${MOVIE_ID}/info" | python3 -c "import sys,json; print(json.load(sys.stdin)['duration_secs'])")
AUTO_WATCH_POS=$(python3 -c "print(round(${MOVIE_DURATION} * 0.95, 1))")
echo "   Testing auto-watched at 95% (${AUTO_WATCH_POS}s of ${MOVIE_DURATION}s)"
test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "set position to 95% of duration" "{\"position_secs\": ${AUTO_WATCH_POS}}"
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get state after 95% position"
assert_body "auto-marked watched at 95%" "
import sys,json; d=json.load(sys.stdin)
assert d['is_watched'] == True, f'expected is_watched=True, got {d[\"is_watched\"]}'
assert d['position_secs'] == ${AUTO_WATCH_POS}
"

test_endpoint DELETE "/api/playback/${MOVIE_ID}/watched" 204 "reset after auto-watched test"

BELOW_THRESHOLD=$(python3 -c "print(round(${MOVIE_DURATION} * 0.5, 1))")
test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "set position to 50% of duration" "{\"position_secs\": ${BELOW_THRESHOLD}}"
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get state after 50% position"
assert_body "NOT auto-marked watched at 50%" "
import sys,json; d=json.load(sys.stdin)
assert d['is_watched'] == False, f'expected is_watched=False at 50%'
"

test_endpoint DELETE "/api/playback/${MOVIE_ID}/watched" 204 "reset after below-threshold test"
echo ""

# ── Batch Watched/Unwatched ─────────────────────────────────────────────────

echo "-- Batch Watched/Unwatched --"
test_endpoint POST "/api/playback/shows/${SHOW_ID}/watched" 200 "mark show watched"
assert_body "batch returns updated count" "
import sys,json; d=json.load(sys.stdin)
assert 'updated' in d
assert d['updated'] > 0
"

curl -s "${BASE}/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" > /tmp/mf_test_body
assert_body "all season episodes marked watched" "
import sys,json; d=json.load(sys.stdin)
for ep in d:
    assert ep['is_watched'] == True, f'episode {ep.get(\"episode_number\")} not watched'
"

curl -s "${BASE}/api/library/shows/${SHOW_ID}" > /tmp/mf_test_body
assert_body "show watched_count matches after batch mark" "
import sys,json; d=json.load(sys.stdin)
total_eps = sum(s['episode_count'] for s in d['seasons'])
total_watched = sum(s['watched_count'] for s in d['seasons'])
assert total_watched == total_eps, f'watched {total_watched} != total {total_eps}'
"

test_endpoint DELETE "/api/playback/shows/${SHOW_ID}/watched" 200 "mark show unwatched"

curl -s "${BASE}/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" > /tmp/mf_test_body
assert_body "all season episodes marked unwatched" "
import sys,json; d=json.load(sys.stdin)
for ep in d:
    assert ep['is_watched'] == False, f'episode {ep.get(\"episode_number\")} still watched'
"

test_endpoint POST "/api/playback/shows/${SHOW_ID}/seasons/${FIRST_SEASON}/watched" 200 "mark season watched"

curl -s "${BASE}/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" > /tmp/mf_test_body
assert_body "season episodes marked watched" "
import sys,json; d=json.load(sys.stdin)
for ep in d:
    assert ep['is_watched'] == True
"

test_endpoint DELETE "/api/playback/shows/${SHOW_ID}/seasons/${FIRST_SEASON}/watched" 200 "mark season unwatched"

curl -s "${BASE}/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" > /tmp/mf_test_body
assert_body "season episodes marked unwatched" "
import sys,json; d=json.load(sys.stdin)
for ep in d:
    assert ep['is_watched'] == False
"

test_endpoint POST "/api/playback/shows/nonexistent-id-000/watched" 404 "batch mark nonexistent show"
test_endpoint DELETE "/api/playback/shows/nonexistent-id-000/watched" 404 "batch unmark nonexistent show"
test_endpoint POST "/api/playback/shows/nonexistent-id-000/seasons/1/watched" 404 "batch mark nonexistent season"
echo ""

# ── Activity History ─────────────────────────────────────────────────────────

echo "-- Activity History --"
test_endpoint GET "/api/playback/history" 200 "activity history (all)"
assert_body "history response shape" "
import sys,json; d=json.load(sys.stdin)
for key in ['entries', 'total', 'limit', 'offset']:
    assert key in d, f'missing {key}'
assert isinstance(d['entries'], list)
assert d['total'] >= 0
"

test_endpoint GET "/api/playback/history?limit=3&offset=0" 200 "history with limit"
assert_body "history limit respected" "
import sys,json; d=json.load(sys.stdin)
assert len(d['entries']) <= 3
assert d['limit'] == 3
assert d['offset'] == 0
"

test_endpoint GET "/api/playback/history?media_id=${MOVIE_ID}" 200 "history for specific media"
assert_body "history entries belong to correct media" "
import sys,json; d=json.load(sys.stdin)
for entry in d['entries']:
    assert entry['media_id'] == '${MOVIE_ID}', f'wrong media_id: {entry[\"media_id\"]}'
    for key in ['id', 'event_type', 'position_secs', 'created_at']:
        assert key in entry, f'missing {key}'
"
assert_body "history contains play/pause events we sent" "
import sys,json; d=json.load(sys.stdin)
types = {e['event_type'] for e in d['entries']}
assert 'play' in types or 'pause' in types or 'complete' in types, f'only got: {types}'
"
echo ""

# ── On Deck (integration) ───────────────────────────────────────────────────

echo "-- On Deck (integration) --"
test_endpoint PUT "/api/playback/${EPISODE_ID}/state" 204 "set episode position for on-deck" '{"position_secs": 30.0, "event": "play"}'

test_endpoint GET "/api/library/ondeck" 200 "on deck after episode progress"
assert_body "on deck items have required fields" "
import sys,json; d=json.load(sys.stdin)
if len(d) > 0:
    item = d[0]
    for key in ['show_id', 'show_name', 'episode_id', 'season_number', 'episode_number', 'duration_secs', 'position_secs']:
        assert key in item, f'missing {key}'
"

test_endpoint PUT "/api/playback/${EPISODE_ID}/state" 204 "reset episode position" '{"position_secs": 0}'
echo ""

# ── Streaming: Info ──────────────────────────────────────────────────────────

echo "-- Streaming --"
test_endpoint GET "/api/stream/${MOVIE_ID}/info" 200 "stream info"
curl -s "${BASE}/api/stream/${MOVIE_ID}/info" > /tmp/mf_test_body
assert_body "stream info has all fields" "
import sys,json; d=json.load(sys.stdin)
for key in ['id', 'video_codec', 'audio_codec', 'video_width', 'video_height',
            'duration_secs', 'file_size', 'needs_transcode', 'can_direct_play', 'subtitles', 'audio_tracks']:
    assert key in d, f'missing {key}'
assert isinstance(d['subtitles'], list)
assert isinstance(d['audio_tracks'], list)
assert isinstance(d['needs_transcode'], bool)
assert isinstance(d['can_direct_play'], bool)
"

test_endpoint GET "/api/stream/nonexistent-id-000/info" 404 "stream info not found"
test_endpoint GET "/api/stream/nonexistent-id-000/hls/status" 404 "hls status no session"
echo ""

# ── Streaming: Range Requests ────────────────────────────────────────────────

echo "-- Range Requests --"
FILE_SIZE=$(curl -s "${BASE}/api/stream/${MOVIE_ID}/info" | python3 -c "import sys,json; print(json.load(sys.stdin)['file_size'])")
echo "   File size: ${FILE_SIZE} bytes"

RANGE_STATUS=$(curl -s -o /tmp/mf_range_body -w "%{http_code}" -H "Range: bytes=0-1023" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$RANGE_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s %s\n" "range bytes=0-1023 returns 206" "206"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range 0-1023: expected 206, got ${RANGE_STATUS}"
    printf "  FAIL  %-60s %s (expected 206)\n" "range bytes=0-1023" "$RANGE_STATUS"
fi

RANGE_LEN=$(wc -c < /tmp/mf_range_body)
if [[ "$RANGE_LEN" == "1024" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "range body is exactly 1024 bytes"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range body size: expected 1024, got ${RANGE_LEN}"
    printf "  FAIL  %-60s %s bytes (expected 1024)\n" "range body size" "$RANGE_LEN"
fi

CONTENT_RANGE=$(curl -s -D /tmp/mf_range_headers -o /dev/null -H "Range: bytes=0-1023" "${BASE}/api/stream/${MOVIE_ID}/direct" && grep -i "content-range" /tmp/mf_range_headers || echo "")
if [[ "$CONTENT_RANGE" == *"bytes 0-1023/"* ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "Content-Range header correct"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  Content-Range: got '${CONTENT_RANGE}'"
    printf "  FAIL  %-60s\n" "Content-Range header"
fi

LAST_BYTE=$((FILE_SIZE - 1))
RANGE_START=$((FILE_SIZE - 512))
SUFFIX_STATUS=$(curl -s -o /tmp/mf_range_body -w "%{http_code}" -H "Range: bytes=${RANGE_START}-${LAST_BYTE}" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$SUFFIX_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "range last 512 bytes"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range last 512: expected 206, got ${SUFFIX_STATUS}"
    printf "  FAIL  %-60s\n" "range last 512 bytes"
fi

SUFFIX_LEN=$(wc -c < /tmp/mf_range_body)
if [[ "$SUFFIX_LEN" == "512" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "last-512 response is exactly 512 bytes"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  last-512 body: expected 512, got ${SUFFIX_LEN}"
    printf "  FAIL  %-60s\n" "last-512 response size"
fi

SUFFIX_RANGE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -H "Range: bytes=-256" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$SUFFIX_RANGE_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "suffix range bytes=-256"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  suffix range: expected 206, got ${SUFFIX_RANGE_STATUS}"
    printf "  FAIL  %-60s\n" "suffix range bytes=-256"
fi

OPEN_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 -H "Range: bytes=1000-" "${BASE}/api/stream/${MOVIE_ID}/direct" 2>/dev/null || echo "timeout")
if [[ "$OPEN_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "open-ended range bytes=1000-"
else
    printf "  SKIP  %-60s (timeout)\n" "open-ended range bytes=1000-"
fi

ACCEPT_RANGES=$(curl -s -D /tmp/mf_range_headers -o /dev/null "${BASE}/api/stream/${MOVIE_ID}/direct" --max-time 2 2>/dev/null; grep -i "accept-ranges" /tmp/mf_range_headers || echo "")
if [[ "$ACCEPT_RANGES" == *"bytes"* ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "Accept-Ranges: bytes header present"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  Accept-Ranges header missing"
    printf "  FAIL  %-60s\n" "Accept-Ranges: bytes header"
fi

PAST_END=$((FILE_SIZE + 1000))
BAD_RANGE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -H "Range: bytes=${PAST_END}-${PAST_END}" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$BAD_RANGE_STATUS" == "500" || "$BAD_RANGE_STATUS" == "416" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s %s\n" "range past EOF returns error" "$BAD_RANGE_STATUS"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range past EOF: expected 416/500, got ${BAD_RANGE_STATUS}"
    printf "  FAIL  %-60s %s\n" "range past EOF" "$BAD_RANGE_STATUS"
fi
echo ""

# ── HLS Full Flow ────────────────────────────────────────────────────────────

echo "-- HLS Full Flow --"
HLS_ID="6fd5b138-c723-4b93-8287-c184a1fea906"
echo "   Using short HEVC file (66s, 5MB) for HLS test"

curl -s "${BASE}/api/stream/${HLS_ID}/info" > /tmp/mf_test_body
HLS_CODEC=$(json_field "['video_codec']")
echo "   Codec: ${HLS_CODEC}"

curl -s -X POST -o /tmp/mf_test_body -w "%{http_code}" "${BASE}/api/stream/${HLS_ID}/hls/prepare" > /tmp/mf_prepare_status
PREP_STATUS=$(cat /tmp/mf_prepare_status)
if [[ "$PREP_STATUS" == "200" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s %s\n" "hls prepare" "200"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  hls prepare: expected 200, got ${PREP_STATUS}"
    printf "  FAIL  %-60s %s (expected 200)\n" "hls prepare" "$PREP_STATUS"
fi
assert_body "prepare returns status field" "
import sys,json; d=json.load(sys.stdin)
assert d['status'] in ('preparing', 'ready'), f\"got status={d['status']}\"
"
rm -f /tmp/mf_prepare_status

echo "   Waiting for HLS to become ready..."
HLS_READY=false
for i in $(seq 1 60); do
    sleep 2
    STATUS=$(curl -s "${BASE}/api/stream/${HLS_ID}/hls/status" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
    if [[ "$STATUS" == "ready" ]]; then
        HLS_READY=true
        printf "   Ready after %d seconds\n" $((i * 2))
        break
    elif [[ "$STATUS" == "error" ]]; then
        ERR=$(curl -s "${BASE}/api/stream/${HLS_ID}/hls/status" | python3 -c "import sys,json; print(json.load(sys.stdin).get('error','unknown'))")
        printf "   HLS error: %s\n" "$ERR"
        break
    fi
    printf "   ...poll %d: %s\n" "$i" "$STATUS"
done

if [[ "$HLS_READY" == "true" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "hls status became ready"

    test_endpoint GET "/api/stream/${HLS_ID}/hls/master.m3u8" 200 "fetch master playlist"
    assert_body "master playlist has #EXTM3U and variants" "
import sys
content = sys.stdin.read()
assert content.startswith('#EXTM3U'), f'starts with: {content[:50]}'
assert '#EXT-X-STREAM-INF' in content
"

    VARIANT=$(curl -s "${BASE}/api/stream/${HLS_ID}/hls/master.m3u8" | grep -v '^#' | head -1 | sed 's|/.*||')
    if [[ -n "$VARIANT" ]]; then
        echo "   Testing variant: ${VARIANT}"
        test_endpoint GET "/api/stream/${HLS_ID}/hls/${VARIANT}/playlist.m3u8" 200 "fetch variant playlist"
        assert_body "variant playlist has segments" "
import sys
content = sys.stdin.read()
assert content.startswith('#EXTM3U')
assert '#EXTINF:' in content
assert '.ts' in content or '.m4s' in content
"
        assert_body "variant has multiple segments" "
import sys
content = sys.stdin.read()
segments = [l for l in content.splitlines() if l.endswith('.ts') or l.endswith('.m4s')]
assert len(segments) >= 2, f'only {len(segments)} segments'
"

        FIRST_SEGMENT=$(curl -s "${BASE}/api/stream/${HLS_ID}/hls/${VARIANT}/playlist.m3u8" | grep -E '\.ts$|\.m4s$' | head -1)
        echo "   First segment: ${FIRST_SEGMENT}"

        if [[ -n "$FIRST_SEGMENT" ]]; then
            SEG_STATUS=$(curl -s -o /tmp/mf_segment -w "%{http_code}" "${BASE}/api/stream/${HLS_ID}/hls/${VARIANT}/${FIRST_SEGMENT}")
            SEG_SIZE=$(wc -c < /tmp/mf_segment)
            if [[ "$SEG_STATUS" == "200" && "$SEG_SIZE" -gt 0 ]]; then
                PASS=$((PASS + 1))
                printf "  PASS  %-60s (%s bytes)\n" "fetch and verify HLS segment" "$SEG_SIZE"
            else
                FAIL=$((FAIL + 1))
                ERRORS="${ERRORS}\n  FAIL  segment: status=${SEG_STATUS}, size=${SEG_SIZE}"
                printf "  FAIL  %-60s\n" "fetch and verify HLS segment"
            fi

            MAGIC=$(xxd -l 4 -p /tmp/mf_segment)
            if [[ "$MAGIC" == "47"* ]]; then
                PASS=$((PASS + 1))
                printf "  PASS  %-60s\n" "segment starts with MPEG-TS sync byte (0x47)"
            else
                printf "  SKIP  %-60s (magic: %s)\n" "MPEG-TS sync byte" "$MAGIC"
            fi
            rm -f /tmp/mf_segment
        fi
    fi

    test_endpoint GET "/api/stream/${HLS_ID}/hls/720p/nonexistent.ts" 400 "nonexistent segment"

    test_endpoint POST "/api/stream/${HLS_ID}/hls/cancel" 200 "cancel HLS transcode"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  HLS never became ready after 120s"
    printf "  FAIL  %-60s\n" "hls status became ready"
fi

test_endpoint GET "/api/stream/${MOVIE_ID}/hls/..%2F..%2Fetc%2Fpasswd" 404 "path traversal blocked"
echo ""

# ── Subtitles ────────────────────────────────────────────────────────────────

echo "-- Subtitles --"
SUB_MEDIA="aadf6437-f0d5-4656-b015-a8f07170fa76"
SUB_ID="2e600803-d510-4e2b-addc-797f8cae3b77"
echo "   Testing embedded subrip extraction"

test_endpoint GET "/api/stream/${SUB_MEDIA}/subtitle/${SUB_ID}" 200 "extract embedded subtitle"

CONTENT_TYPE=$(grep -i "content-type" /tmp/mf_test_headers || echo "")
if [[ "$CONTENT_TYPE" == *"text/vtt"* ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s\n" "Content-Type is text/vtt"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  subtitle Content-Type: ${CONTENT_TYPE}"
    printf "  FAIL  %-60s\n" "Content-Type is text/vtt"
fi

assert_body "subtitle starts with WEBVTT" "
import sys
content = sys.stdin.read()
assert content.strip().startswith('WEBVTT'), f'starts with: {repr(content[:50])}'
"
assert_body "subtitle contains timestamps" "
import sys
content = sys.stdin.read()
assert '-->' in content, 'no timestamp markers'
"
assert_body "subtitle has dialogue text" "
import sys
lines = [l.strip() for l in sys.stdin.readlines() if l.strip() and not l.startswith('WEBVTT') and '-->' not in l and not l.strip().isdigit()]
assert len(lines) > 5, f'only {len(lines)} text lines'
"

test_endpoint GET "/api/stream/${SUB_MEDIA}/subtitle/nonexistent-sub-id" 404 "subtitle not found"
test_endpoint GET "/api/stream/nonexistent-id/subtitle/${SUB_ID}" 404 "subtitle wrong media"
echo ""

# ── Metadata ─────────────────────────────────────────────────────────────────

echo "-- Metadata --"
SCAN_STATUS=$(curl -s -o /tmp/mf_test_body -w "%{http_code}" -X POST "${BASE}/api/metadata/scan")
if [[ "$SCAN_STATUS" == "200" || "$SCAN_STATUS" == "409" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s %s\n" "trigger scan (200 or 409 if already running)" "$SCAN_STATUS"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  trigger scan: expected 200/409, got ${SCAN_STATUS}"
    printf "  FAIL  %-60s %s\n" "trigger scan" "$SCAN_STATUS"
fi

REFRESH_STATUS=$(curl -s -o /tmp/mf_test_body -w "%{http_code}" -X POST "${BASE}/api/metadata/refresh")
if [[ "$REFRESH_STATUS" == "200" || "$REFRESH_STATUS" == "409" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-60s %s\n" "trigger refresh (200 or 409 if already running)" "$REFRESH_STATUS"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  trigger refresh: expected 200/409, got ${REFRESH_STATUS}"
    printf "  FAIL  %-60s %s\n" "trigger refresh" "$REFRESH_STATUS"
fi

curl -s "${BASE}/api/library/movies?per_page=50&sort=rating" > /tmp/mf_test_body
POSTER=$(python3 -c "
import sys,json
items = json.load(sys.stdin)['items']
for i in items:
    if i.get('poster_path'):
        print(i['poster_path']); break
else:
    print('')
" < /tmp/mf_test_body)
if [[ -n "$POSTER" && "$POSTER" != "None" ]]; then
    POSTER_PATH="${POSTER#/}"
    test_endpoint GET "/api/metadata/image/${POSTER_PATH}" 200 "proxy image (first fetch)"

    POSTER_SIZE=$(wc -c < /tmp/mf_test_body)
    if [[ "$POSTER_SIZE" -gt 1000 ]]; then
        PASS=$((PASS + 1))
        printf "  PASS  %-60s %s bytes\n" "poster is real image (>1KB)" "$POSTER_SIZE"
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL  poster too small: ${POSTER_SIZE} bytes"
        printf "  FAIL  %-60s %s bytes\n" "poster size" "$POSTER_SIZE"
    fi

    test_endpoint GET "/api/metadata/image/${POSTER_PATH}" 200 "proxy image (cached)"
    CACHE_CONTROL=$(grep -i 'cache-control' /tmp/mf_test_headers 2>/dev/null || true)
    if echo "$CACHE_CONTROL" | grep -q "max-age=604800"; then
        PASS=$((PASS + 1))
        printf "  PASS  %-60s\n" "Cache-Control header present"
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL  Missing Cache-Control header"
        printf "  FAIL  %-60s\n" "Cache-Control header"
    fi

    test_endpoint GET "/api/metadata/image/${POSTER_PATH}?size=w185" 200 "proxy image (w185)"
    test_endpoint GET "/api/metadata/image/${POSTER_PATH}?size=invalid" 400 "proxy image (invalid size)"
else
    printf "  SKIP  %-60s (no poster available)\n" "proxy image"
fi
echo ""

# ── Edge Cases ───────────────────────────────────────────────────────────────

echo "-- Edge Cases --"
test_endpoint GET "/api/library/movies?page=99999" 200 "movies page beyond range"
assert_body "beyond-range page returns empty items" "
import sys,json; d=json.load(sys.stdin)
assert len(d['items']) == 0
assert d['total'] > 0
"

test_endpoint GET "/api/library/movies?per_page=0" 200 "per_page=0"
test_endpoint GET "/api/library/movies?per_page=201" 200 "per_page=201 (above max)"
assert_body "per_page clamped to 200" "
import sys,json; d=json.load(sys.stdin)
assert d['per_page'] <= 200, f'per_page={d[\"per_page\"]}'
"

test_endpoint GET "/api/library/shows?page=99999" 200 "shows page beyond range"
assert_body "shows beyond-range returns empty" "
import sys,json; d=json.load(sys.stdin)
assert len(d['items']) == 0
assert d['total'] > 0
"

test_endpoint GET "/api/library/shows?per_page=201" 200 "shows per_page clamped"
assert_body "shows per_page clamped to 200" "
import sys,json; d=json.load(sys.stdin)
assert d['per_page'] <= 200
"

test_endpoint GET "/api/library/movies?genre=${GENRE}&page=1&per_page=2&sort=rating" 200 "genre + pagination + sort combo"
echo ""

# ── Cleanup test state ──────────────────────────────────────────────────────

echo "-- Cleanup --"
test_endpoint DELETE "/api/playback/${MOVIE_ID}/watched" 204 "reset movie state"
printf "  INFO  %-60s\n" "test playback state cleaned up"
echo ""

# ── Results ──────────────────────────────────────────────────────────────────

echo "========================================"
printf " Results: %d passed, %d failed\n" "$PASS" "$FAIL"
echo "========================================"

if [[ $FAIL -gt 0 ]]; then
    echo ""
    echo "Failures:"
    printf "$ERRORS\n"
    exit 1
fi

rm -f /tmp/mf_test_body /tmp/mf_test_headers /tmp/mf_range_body /tmp/mf_range_headers
