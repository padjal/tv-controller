#!/usr/bin/env bash
# Server playback endpoints: play -> pause -> resume -> stop, plus play-all
# Requires: curl, jq
#
# Needs at least one registered device and one indexed video. The device must
# be reachable — run the real pi-agent, or any stub answering POST /play,
# /stop, /pause and /resume on port 8080.
#
# Usage:
#   SERVER_HOST=192.168.1.10 ./playback.sh
#   ./playback.sh 192.168.1.10

set -euo pipefail

SERVER_HOST="${1:-${SERVER_HOST:-127.0.0.1}}"
SERVER_PORT="${SERVER_PORT:-8000}"
BASE="http://${SERVER_HOST}:${SERVER_PORT}/api"

pass()    { printf "[PASS] %s\n" "$1"; }
fail()    { printf "[FAIL] %s\n" "$1"; exit 1; }
section() { printf "\n=== %s ===\n" "$1"; }

state_of() { curl -sf "$BASE/devices/$1" | jq -r '.state'; }
video_of() { curl -sf "$BASE/devices/$1" | jq -r '.current_video'; }

echo "server playback endpoints -> $BASE"

devices=$(curl -sf "$BASE/devices")
videos=$(curl -sf "$BASE/videos")
[[ "$(echo "$devices" | jq 'length')" != "0" ]] || fail "no devices registered"
[[ "$(echo "$videos"  | jq 'length')" != "0" ]] || fail "no videos indexed"

device=$(echo "$devices" | jq -r '.[0].id')
device_name=$(echo "$devices" | jq -r '.[0].name')
video=$(echo "$videos" | jq -r '.[0].id')
filename=$(echo "$videos" | jq -r '.[0].filename')
echo "device: $device_name ($device)"
echo "video:  $filename ($video)"

section "play"
curl -sf -X POST "$BASE/playback/play" \
  -H "Content-Type: application/json" \
  -d "{\"device_ids\":[\"$device\"],\"video_id\":\"$video\"}" | jq .
s=$(state_of "$device"); [[ "$s" == "Playing" ]] || fail "expected Playing, got $s"
v=$(video_of "$device"); [[ "$v" == "$filename" ]] || fail "expected $filename, got $v"
pass "state=Playing, current_video=$filename"

section "pause"
curl -sf -X POST "$BASE/playback/pause" \
  -H "Content-Type: application/json" -d "{\"device_ids\":[\"$device\"]}" | jq .
s=$(state_of "$device"); [[ "$s" == "Paused" ]] || fail "expected Paused, got $s"
v=$(video_of "$device"); [[ "$v" == "$filename" ]] || fail "pause lost the video: $v"
pass "state=Paused, video retained"

section "resume"
curl -sf -X POST "$BASE/playback/resume" \
  -H "Content-Type: application/json" -d "{\"device_ids\":[\"$device\"]}" | jq .
s=$(state_of "$device"); [[ "$s" == "Playing" ]] || fail "expected Playing, got $s"
pass "state=Playing"

section "stop"
curl -sf -X POST "$BASE/playback/stop" \
  -H "Content-Type: application/json" -d "{\"device_ids\":[\"$device\"]}" | jq .
s=$(state_of "$device"); [[ "$s" == "Idle" ]] || fail "expected Idle, got $s"
v=$(video_of "$device"); [[ "$v" == "null" ]] || fail "stop should clear the video, got $v"
pass "state=Idle, video cleared"

section "play-all"
curl -sf -X POST "$BASE/playback/play-all" \
  -H "Content-Type: application/json" -d "{\"video_id\":\"$video\"}" | jq .
s=$(state_of "$device"); [[ "$s" == "Playing" ]] || fail "expected Playing, got $s"
pass "play-all reached the device"

curl -sf -X POST "$BASE/playback/stop" \
  -H "Content-Type: application/json" -d "{\"device_ids\":[\"$device\"]}" > /dev/null

section "empty device_ids (expect 400)"
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/playback/play" \
  -H "Content-Type: application/json" -d "{\"device_ids\":[],\"video_id\":\"$video\"}")
[[ "$code" == "400" ]] || fail "expected 400, got $code"
pass "empty device_ids -> 400"

section "unknown video (expect 404)"
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/playback/play" \
  -H "Content-Type: application/json" \
  -d "{\"device_ids\":[\"$device\"],\"video_id\":\"00000000-0000-4000-8000-000000000000\"}")
[[ "$code" == "404" ]] || fail "expected 404, got $code"
pass "unknown video -> 404"

section "unknown device (expect 502, nothing reachable)"
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/playback/play" \
  -H "Content-Type: application/json" \
  -d "{\"device_ids\":[\"00000000-0000-4000-8000-000000000000\"],\"video_id\":\"$video\"}")
[[ "$code" == "502" ]] || fail "expected 502 when every target fails, got $code"
pass "all-targets-failed -> 502"

printf "\nAll tests passed.\n"
