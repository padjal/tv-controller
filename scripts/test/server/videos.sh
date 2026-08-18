#!/usr/bin/env bash
# Server video endpoints: list -> metadata -> file download -> Range request
# Requires: curl, jq
#
# Needs at least one video in the server's VIDEOS_DIR. Run the server, drop a
# file in, wait for the scanner (~2s debounce), then run this.
#
# Usage:
#   SERVER_HOST=192.168.1.10 ./videos.sh
#   ./videos.sh 192.168.1.10

set -euo pipefail

SERVER_HOST="${1:-${SERVER_HOST:-127.0.0.1}}"
SERVER_PORT="${SERVER_PORT:-8000}"
BASE="http://${SERVER_HOST}:${SERVER_PORT}"

pass()    { printf "[PASS] %s\n" "$1"; }
fail()    { printf "[FAIL] %s\n" "$1"; exit 1; }
skip()    { printf "[SKIP] %s\n" "$1"; }
section() { printf "\n=== %s ===\n" "$1"; }

echo "server video endpoints -> $BASE"

section "list"
videos=$(curl -sf "$BASE/api/videos")
echo "$videos" | jq .
count=$(echo "$videos" | jq 'length')
pass "list returned $count video(s)"

if [[ "$count" == "0" ]]; then
  skip "no videos indexed; drop a file into VIDEOS_DIR and re-run to test file serving"
  exit 0
fi

id=$(echo "$videos" | jq -r '.[0].id')
filename=$(echo "$videos" | jq -r '.[0].filename')
size=$(echo "$videos" | jq -r '.[0].size_bytes')
echo "using: $filename ($size bytes, $id)"

section "metadata by id"
got=$(curl -sf "$BASE/api/videos/$id" | jq -r '.filename')
[[ "$got" == "$filename" ]] || fail "expected $filename, got $got"
pass "metadata matches"

section "unknown id (expect 404)"
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/videos/00000000-0000-4000-8000-000000000000")
[[ "$code" == "404" ]] || fail "expected 404 for unknown id, got $code"
pass "unknown id -> 404"

# Percent-encode the filename the same way the server does when it hands a URL
# to an agent, so names with spaces or '#' resolve.
encoded=$(jq -rn --arg s "$filename" '$s|@uri')

section "whole file"
headers=$(curl -sfI "$BASE/videos/$encoded")
echo "$headers"
echo "$headers" | grep -qi '^accept-ranges: bytes' || fail "server does not advertise Range support"
pass "file reachable and advertises Accept-Ranges"

section "range request (expect 206)"
code=$(curl -s -o /dev/null -w '%{http_code}' -r 0-99 "$BASE/videos/$encoded")
[[ "$code" == "206" ]] || fail "expected 206 for a range request, got $code"
bytes=$(curl -s -r 0-99 "$BASE/videos/$encoded" | wc -c | tr -d ' ')
[[ "$bytes" == "100" ]] || fail "expected 100 bytes, got $bytes"
pass "range request returned exactly 100 bytes (mpv can seek)"

section "unsatisfiable range (expect 416)"
code=$(curl -s -o /dev/null -w '%{http_code}' -r "$((size + 1000))-$((size + 2000))" "$BASE/videos/$encoded")
[[ "$code" == "416" ]] || fail "expected 416 past end of file, got $code"
pass "past-the-end range -> 416"

section "unknown file (expect 404)"
code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/videos/definitely-not-here.mp4")
[[ "$code" == "404" ]] || fail "expected 404, got $code"
pass "unknown file -> 404"

printf "\nAll tests passed.\n"
