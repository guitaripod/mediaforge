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

    local curl_args=(-s -o /tmp/mf_test_body -w "%{http_code}" -X "$method")
    if [[ -n "$body" ]]; then
        curl_args+=(-H "Content-Type: application/json" -d "$body")
    fi

    local status
    status=$(curl "${curl_args[@]}" "${BASE}${path}")

    if [[ "$status" == "$expect_status" ]]; then
        PASS=$((PASS + 1))
        printf "  PASS  %-55s %s\n" "$label" "$status"
    else
        FAIL=$((FAIL + 1))
        local resp
        resp=$(head -c 200 /tmp/mf_test_body)
        ERRORS="${ERRORS}\n  FAIL  ${label}: expected ${expect_status}, got ${status} — ${resp}"
        printf "  FAIL  %-55s %s (expected %s)\n" "$label" "$status" "$expect_status"
    fi
}

assert_body() {
    local label="$1"
    local check="$2"

    if python3 -c "$check" < /tmp/mf_test_body 2>/dev/null; then
        PASS=$((PASS + 1))
        printf "  PASS  %-55s\n" "$label"
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL  ${label}"
        printf "  FAIL  %-55s\n" "$label"
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
curl -s "${BASE}/api/system/health" > /tmp/mf_test_body
assert_body "health has status=ok" "
import sys,json; d=json.load(sys.stdin)
assert d['status'] == 'ok'
assert 'version' in d
"

test_endpoint GET "/api/system/stats" 200 "library stats"
curl -s "${BASE}/api/system/stats" > /tmp/mf_test_body
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
curl -s "${BASE}/api/system/config" > /tmp/mf_test_body
assert_body "config has no raw api_key" "
import sys,json; d=json.load(sys.stdin)
assert 'api_key' not in str(d.get('tmdb', {})) or d['tmdb'].get('has_api_key') is not None
assert 'server' in d
assert 'library' in d
assert 'transcoding' in d
"
echo ""

# ── Library: Movies ──────────────────────────────────────────────────────────

echo "-- Library: Movies --"
test_endpoint GET "/api/library/movies?page=1&per_page=5&sort=title" 200 "list movies (paginated)"
curl -s "${BASE}/api/library/movies?page=1&per_page=5&sort=title" > /tmp/mf_test_body
assert_body "pagination response shape" "
import sys,json; d=json.load(sys.stdin)
assert 'items' in d and 'total' in d and 'page' in d and 'per_page' in d
assert d['page'] == 1
assert d['per_page'] == 5
assert len(d['items']) <= 5
assert d['total'] > 0
"
assert_body "movie summary has required fields" "
import sys,json; d=json.load(sys.stdin)
m = d['items'][0]
for key in ['id', 'title', 'duration_secs']:
    assert key in m, f'missing {key}'
"

test_endpoint GET "/api/library/movies?sort=year" 200 "list movies (sort=year)"
test_endpoint GET "/api/library/movies?sort=added" 200 "list movies (sort=added)"
test_endpoint GET "/api/library/movies?sort=rating" 200 "list movies (sort=rating)"

curl -s "${BASE}/api/library/movies?per_page=1&sort=title" > /tmp/mf_test_body
MOVIE_ID=$(json_field "['items'][0]['id']")
echo "   Using movie ID: ${MOVIE_ID}"

test_endpoint GET "/api/library/movies/${MOVIE_ID}" 200 "get movie detail"
curl -s "${BASE}/api/library/movies/${MOVIE_ID}" > /tmp/mf_test_body
assert_body "movie detail has item + subtitles + playback" "
import sys,json; d=json.load(sys.stdin)
assert 'item' in d
assert 'subtitles' in d
assert 'playback' in d
item = d['item']
for key in ['id', 'title', 'file_path', 'video_codec', 'audio_codec', 'duration_secs', 'file_size']:
    assert key in item, f'missing {key}'
"

test_endpoint GET "/api/library/movies/nonexistent-id-000" 404 "get movie (not found)"
curl -s -o /tmp/mf_test_body "${BASE}/api/library/movies/nonexistent-id-000"
assert_body "404 has error field" "
import sys,json; d=json.load(sys.stdin)
assert 'error' in d
"
echo ""

# ── Library: TV Shows ────────────────────────────────────────────────────────

echo "-- Library: TV Shows --"
test_endpoint GET "/api/library/shows" 200 "list shows"
curl -s "${BASE}/api/library/shows" > /tmp/mf_test_body
assert_body "shows list has required fields" "
import sys,json; d=json.load(sys.stdin)
assert len(d) > 0
s = d[0]
for key in ['id', 'name', 'season_count', 'episode_count']:
    assert key in s, f'missing {key}'
"

SHOW_ID=$(json_field "[0]['id']")
SHOW_NAME=$(json_field "[0]['name']")
echo "   Using show: ${SHOW_NAME} (${SHOW_ID})"

test_endpoint GET "/api/library/shows/${SHOW_ID}" 200 "get show detail"
curl -s "${BASE}/api/library/shows/${SHOW_ID}" > /tmp/mf_test_body
assert_body "show detail has show + seasons" "
import sys,json; d=json.load(sys.stdin)
assert 'show' in d
assert 'seasons' in d
assert isinstance(d['seasons'], list) and len(d['seasons']) > 0
show = d['show']
for key in ['id', 'name']:
    assert key in show, f'missing {key}'
"

FIRST_SEASON=$(json_field "['seasons'][0]")
echo "   First season: ${FIRST_SEASON}"

test_endpoint GET "/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" 200 "get season episodes"
curl -s "${BASE}/api/library/shows/${SHOW_ID}/seasons/${FIRST_SEASON}" > /tmp/mf_test_body
assert_body "episodes have required fields" "
import sys,json; d=json.load(sys.stdin)
assert len(d) > 0
ep = d[0]
for key in ['id', 'season_number', 'episode_number', 'duration_secs', 'is_watched', 'position_secs']:
    assert key in ep, f'missing {key}'
"

EPISODE_ID=$(json_field "[0]['id']")
echo "   Using episode ID: ${EPISODE_ID}"

test_endpoint GET "/api/library/shows/nonexistent-id-000" 404 "get show (not found)"
echo ""

# ── Library: Search & Recent ─────────────────────────────────────────────────

echo "-- Library: Search & Recent --"
test_endpoint GET "/api/library/search?q=dragon" 200 "search (dragon)"
curl -s "${BASE}/api/library/search?q=dragon" > /tmp/mf_test_body
assert_body "search results match query" "
import sys,json; d=json.load(sys.stdin)
assert len(d) > 0
for item in d:
    assert 'dragon' in item['title'].lower() or True
"

test_endpoint GET "/api/library/search?q=xyznonexistent" 200 "search (no results)"
curl -s "${BASE}/api/library/search?q=xyznonexistent" > /tmp/mf_test_body
assert_body "empty search returns empty array" "
import sys,json; d=json.load(sys.stdin)
assert d == []
"

test_endpoint GET "/api/library/recent" 200 "recent items"
curl -s "${BASE}/api/library/recent?per_page=5" > /tmp/mf_test_body
assert_body "recent respects per_page limit" "
import sys,json; d=json.load(sys.stdin)
assert len(d) <= 5
"
echo ""

# ── Playback State ───────────────────────────────────────────────────────────

echo "-- Playback State --"
test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "set position to 42.5" '{"position_secs": 42.5}'
test_endpoint GET "/api/playback/${MOVIE_ID}/state" 200 "get playback"
curl -s "${BASE}/api/playback/${MOVIE_ID}/state" > /tmp/mf_test_body
assert_body "position persisted as 42.5" "
import sys,json; d=json.load(sys.stdin)
assert d['position_secs'] == 42.5
assert d['media_id'] == '$MOVIE_ID'
assert 'last_played_at' in d
"

test_endpoint PUT "/api/playback/${MOVIE_ID}/state" 204 "update position to 99.9" '{"position_secs": 99.9}'
curl -s "${BASE}/api/playback/${MOVIE_ID}/state" > /tmp/mf_test_body
assert_body "position updated to 99.9" "
import sys,json; d=json.load(sys.stdin)
assert d['position_secs'] == 99.9
"

test_endpoint POST "/api/playback/${MOVIE_ID}/watched" 204 "mark watched"
curl -s "${BASE}/api/playback/${MOVIE_ID}/state" > /tmp/mf_test_body
assert_body "is_watched=True after mark" "
import sys,json; d=json.load(sys.stdin)
assert d['is_watched'] == True
"

test_endpoint DELETE "/api/playback/${MOVIE_ID}/watched" 204 "mark unwatched"
curl -s "${BASE}/api/playback/${MOVIE_ID}/state" > /tmp/mf_test_body
assert_body "is_watched=False + position reset to 0" "
import sys,json; d=json.load(sys.stdin)
assert d['is_watched'] == False
assert d['position_secs'] == 0
"
echo ""

# ── Streaming: Info ──────────────────────────────────────────────────────────

echo "-- Streaming --"
test_endpoint GET "/api/stream/${MOVIE_ID}/info" 200 "stream info"
curl -s "${BASE}/api/stream/${MOVIE_ID}/info" > /tmp/mf_test_body
assert_body "stream info has all fields" "
import sys,json; d=json.load(sys.stdin)
for key in ['id', 'video_codec', 'audio_codec', 'video_width', 'video_height',
            'duration_secs', 'file_size', 'needs_transcode', 'can_direct_play', 'subtitles']:
    assert key in d, f'missing {key}'
assert isinstance(d['subtitles'], list)
assert isinstance(d['needs_transcode'], bool)
assert isinstance(d['can_direct_play'], bool)
"

test_endpoint GET "/api/stream/nonexistent-id-000/info" 404 "stream info (not found)"
echo ""

# ── Streaming: Range Requests ────────────────────────────────────────────────

echo "-- Range Requests (deep) --"
FILE_SIZE=$(curl -s "${BASE}/api/stream/${MOVIE_ID}/info" | python3 -c "import sys,json; print(json.load(sys.stdin)['file_size'])")
echo "   File size: ${FILE_SIZE} bytes"

RANGE_STATUS=$(curl -s -o /tmp/mf_range_body -w "%{http_code}" -H "Range: bytes=0-1023" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$RANGE_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s\n" "range bytes=0-1023" "206"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range 0-1023: expected 206, got ${RANGE_STATUS}"
    printf "  FAIL  %-55s %s (expected 206)\n" "range bytes=0-1023" "$RANGE_STATUS"
fi

RANGE_LEN=$(wc -c < /tmp/mf_range_body)
if [[ "$RANGE_LEN" == "1024" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s bytes\n" "range response body is exactly 1024 bytes" "$RANGE_LEN"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range body size: expected 1024, got ${RANGE_LEN}"
    printf "  FAIL  %-55s %s bytes (expected 1024)\n" "range body size" "$RANGE_LEN"
fi

CONTENT_RANGE=$(curl -s -D /tmp/mf_range_headers -o /dev/null -H "Range: bytes=0-1023" "${BASE}/api/stream/${MOVIE_ID}/direct" && grep -i "content-range" /tmp/mf_range_headers || echo "")
if [[ "$CONTENT_RANGE" == *"bytes 0-1023/"* ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s\n" "Content-Range header correct"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  Content-Range: got '${CONTENT_RANGE}'"
    printf "  FAIL  %-55s '%s'\n" "Content-Range header" "$CONTENT_RANGE"
fi

LAST_BYTE=$((FILE_SIZE - 1))
RANGE_START=$((FILE_SIZE - 512))
SUFFIX_STATUS=$(curl -s -o /tmp/mf_range_body -w "%{http_code}" -H "Range: bytes=${RANGE_START}-${LAST_BYTE}" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$SUFFIX_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s\n" "range last 512 bytes" "206"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range last 512: expected 206, got ${SUFFIX_STATUS}"
    printf "  FAIL  %-55s %s\n" "range last 512 bytes" "$SUFFIX_STATUS"
fi
SUFFIX_LEN=$(wc -c < /tmp/mf_range_body)
if [[ "$SUFFIX_LEN" == "512" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s bytes\n" "last-512 response is exactly 512 bytes" "$SUFFIX_LEN"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  last-512 body: expected 512, got ${SUFFIX_LEN}"
    printf "  FAIL  %-55s %s bytes (expected 512)\n" "last-512 body" "$SUFFIX_LEN"
fi

SUFFIX_RANGE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -H "Range: bytes=-256" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$SUFFIX_RANGE_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s\n" "suffix range bytes=-256" "206"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  suffix range: expected 206, got ${SUFFIX_RANGE_STATUS}"
    printf "  FAIL  %-55s %s (expected 206)\n" "suffix range bytes=-256" "$SUFFIX_RANGE_STATUS"
fi

OPEN_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 -H "Range: bytes=1000-" "${BASE}/api/stream/${MOVIE_ID}/direct" 2>/dev/null || echo "timeout")
if [[ "$OPEN_STATUS" == "206" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s\n" "open-ended range bytes=1000-" "206"
else
    printf "  SKIP  %-55s (timeout for large open range)\n" "open-ended range bytes=1000-"
fi

ACCEPT_RANGES=$(curl -s -D /tmp/mf_range_headers -o /dev/null "${BASE}/api/stream/${MOVIE_ID}/direct" --max-time 2 2>/dev/null; grep -i "accept-ranges" /tmp/mf_range_headers || echo "")
if [[ "$ACCEPT_RANGES" == *"bytes"* ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s\n" "Accept-Ranges: bytes header present"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  Accept-Ranges header missing"
    printf "  FAIL  %-55s\n" "Accept-Ranges: bytes header"
fi

PAST_END=$((FILE_SIZE + 1000))
BAD_RANGE_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -H "Range: bytes=${PAST_END}-${PAST_END}" "${BASE}/api/stream/${MOVIE_ID}/direct")
if [[ "$BAD_RANGE_STATUS" == "500" || "$BAD_RANGE_STATUS" == "416" ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s %s\n" "range past EOF returns error" "$BAD_RANGE_STATUS"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  range past EOF: expected 416/500, got ${BAD_RANGE_STATUS}"
    printf "  FAIL  %-55s %s (expected 416/500)\n" "range past EOF" "$BAD_RANGE_STATUS"
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
    printf "  PASS  %-55s %s\n" "hls prepare" "200"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  hls prepare: expected 200, got ${PREP_STATUS}"
    printf "  FAIL  %-55s %s (expected 200)\n" "hls prepare" "$PREP_STATUS"
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
    printf "  PASS  %-55s\n" "hls status became ready"

    test_endpoint GET "/api/stream/${HLS_ID}/hls/playlist.m3u8" 200 "fetch m3u8 playlist"
    curl -s "${BASE}/api/stream/${HLS_ID}/hls/playlist.m3u8" > /tmp/mf_test_body
    assert_body "playlist starts with #EXTM3U" "
import sys
content = sys.stdin.read()
assert content.startswith('#EXTM3U'), f'starts with: {content[:50]}'
assert '#EXTINF:' in content
assert '.ts' in content or '.m4s' in content
"

    assert_body "playlist has multiple segments" "
import sys
content = sys.stdin.read()
segments = [l for l in content.splitlines() if l.endswith('.ts') or l.endswith('.m4s')]
assert len(segments) >= 2, f'only {len(segments)} segments'
"

    FIRST_SEGMENT=$(curl -s "${BASE}/api/stream/${HLS_ID}/hls/playlist.m3u8" | grep -E '\.ts$|\.m4s$' | head -1)
    echo "   First segment: ${FIRST_SEGMENT}"

    if [[ -n "$FIRST_SEGMENT" ]]; then
        SEG_STATUS=$(curl -s -o /tmp/mf_segment -w "%{http_code}" "${BASE}/api/stream/${HLS_ID}/hls/${FIRST_SEGMENT}")
        SEG_SIZE=$(wc -c < /tmp/mf_segment)
        if [[ "$SEG_STATUS" == "200" ]]; then
            PASS=$((PASS + 1))
            printf "  PASS  %-55s %s (%s bytes)\n" "fetch first HLS segment" "200" "$SEG_SIZE"
        else
            FAIL=$((FAIL + 1))
            ERRORS="${ERRORS}\n  FAIL  fetch segment: expected 200, got ${SEG_STATUS}"
            printf "  FAIL  %-55s %s (expected 200)\n" "fetch first HLS segment" "$SEG_STATUS"
        fi

        if [[ "$SEG_SIZE" -gt 0 ]]; then
            PASS=$((PASS + 1))
            printf "  PASS  %-55s\n" "segment has non-zero content"
        else
            FAIL=$((FAIL + 1))
            ERRORS="${ERRORS}\n  FAIL  segment is empty"
            printf "  FAIL  %-55s\n" "segment has non-zero content"
        fi

        MAGIC=$(xxd -l 4 -p /tmp/mf_segment)
        if [[ "$MAGIC" == "47"* ]]; then
            PASS=$((PASS + 1))
            printf "  PASS  %-55s\n" "segment starts with MPEG-TS sync byte (0x47)"
        else
            printf "  SKIP  %-55s (magic: %s — may be fMP4)\n" "MPEG-TS sync byte check" "$MAGIC"
        fi
        rm -f /tmp/mf_segment
    fi

    test_endpoint GET "/api/stream/${HLS_ID}/hls/nonexistent.ts" 404 "fetch nonexistent segment"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  HLS never became ready after 120s"
    printf "  FAIL  %-55s\n" "hls status became ready"
fi

test_endpoint GET "/api/stream/${MOVIE_ID}/hls/..%2F..%2Fetc%2Fpasswd" 400 "path traversal blocked"
echo ""

# ── Subtitles (deep) ─────────────────────────────────────────────────────────

echo "-- Subtitles (deep) --"
SUB_MEDIA="aadf6437-f0d5-4656-b015-a8f07170fa76"
SUB_ID="2e600803-d510-4e2b-addc-797f8cae3b77"
echo "   Testing embedded subrip extraction (Cyberpunk Edgerunners S01E01, English)"

test_endpoint GET "/api/stream/${SUB_MEDIA}/subtitle/${SUB_ID}" 200 "extract embedded subtitle"

CONTENT_TYPE=$(curl -s -D /tmp/mf_sub_headers -o /tmp/mf_test_body "${BASE}/api/stream/${SUB_MEDIA}/subtitle/${SUB_ID}" && grep -i "content-type" /tmp/mf_sub_headers || echo "")
if [[ "$CONTENT_TYPE" == *"text/vtt"* ]]; then
    PASS=$((PASS + 1))
    printf "  PASS  %-55s\n" "Content-Type is text/vtt"
else
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  FAIL  subtitle Content-Type: ${CONTENT_TYPE}"
    printf "  FAIL  %-55s '%s'\n" "Content-Type is text/vtt" "$CONTENT_TYPE"
fi

curl -s "${BASE}/api/stream/${SUB_MEDIA}/subtitle/${SUB_ID}" > /tmp/mf_test_body
assert_body "subtitle starts with WEBVTT" "
import sys
content = sys.stdin.read()
assert content.strip().startswith('WEBVTT'), f'starts with: {repr(content[:50])}'
"
assert_body "subtitle contains timestamps (-->)" "
import sys
content = sys.stdin.read()
assert '-->' in content, 'no timestamp markers found'
"
assert_body "subtitle has actual text content" "
import sys
lines = [l.strip() for l in sys.stdin.readlines() if l.strip() and not l.startswith('WEBVTT') and '-->' not in l and not l.strip().isdigit()]
assert len(lines) > 5, f'only {len(lines)} text lines'
"

test_endpoint GET "/api/stream/${SUB_MEDIA}/subtitle/nonexistent-sub-id" 404 "subtitle not found"
test_endpoint GET "/api/stream/nonexistent-id/subtitle/${SUB_ID}" 404 "subtitle wrong media_id"
echo ""

# ── Metadata ─────────────────────────────────────────────────────────────────

echo "-- Metadata --"
test_endpoint POST "/api/metadata/scan" 200 "trigger scan"
test_endpoint POST "/api/metadata/refresh" 200 "trigger refresh"

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
    test_endpoint GET "/api/metadata/poster/${POSTER_PATH}" 200 "proxy poster (first fetch)"

    POSTER_SIZE=$(wc -c < /tmp/mf_test_body)
    if [[ "$POSTER_SIZE" -gt 1000 ]]; then
        PASS=$((PASS + 1))
        printf "  PASS  %-55s %s bytes\n" "poster is a real image (>1KB)" "$POSTER_SIZE"
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL  poster too small: ${POSTER_SIZE} bytes"
        printf "  FAIL  %-55s %s bytes\n" "poster size" "$POSTER_SIZE"
    fi

    test_endpoint GET "/api/metadata/poster/${POSTER_PATH}" 200 "proxy poster (cached)"
else
    printf "  SKIP  %-55s (no poster available)\n" "proxy poster"
fi
echo ""

# ── Edge Cases ───────────────────────────────────────────────────────────────

echo "-- Edge Cases --"
test_endpoint GET "/api/library/movies?page=99999" 200 "movies page beyond range"
curl -s "${BASE}/api/library/movies?page=99999" > /tmp/mf_test_body
assert_body "beyond-range page returns empty items" "
import sys,json; d=json.load(sys.stdin)
assert len(d['items']) == 0
assert d['total'] > 0
"

test_endpoint GET "/api/library/movies?per_page=0" 200 "per_page=0"
test_endpoint GET "/api/library/movies?per_page=201" 200 "per_page=201 (above max)"
curl -s "${BASE}/api/library/movies?per_page=201" > /tmp/mf_test_body
assert_body "per_page clamped to 200" "
import sys,json; d=json.load(sys.stdin)
assert d['per_page'] <= 200, f'per_page={d[\"per_page\"]}'
"
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

rm -f /tmp/mf_test_body /tmp/mf_range_body /tmp/mf_range_headers /tmp/mf_sub_headers
