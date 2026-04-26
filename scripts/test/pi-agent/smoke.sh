#!/usr/bin/env bash
# Full pi-agent test sequence: health -> play -> pause -> resume -> stop
# Requires: curl, jq
#
# Usage:
#   PI_HOST=192.168.1.11 ./smoke.sh
#   ./smoke.sh 192.168.1.11 /tmp/test.mp4

set -euo pipefail

PI_HOST="${1:-${PI_HOST:-192.168.1.11}}"
PI_PORT="${PI_PORT:-8080}"
VIDEO="${2:-/tmp/test.mp4}"
BASE="http://${PI_HOST}:${PI_PORT}"

pass()    { printf "[PASS] %s\n" "$1"; }
fail()    { printf "[FAIL] %s\n" "$1"; exit 1; }
section() { printf "\n=== %s ===\n" "$1"; }

echo "pi-agent smoke test -> $BASE"
echo "video:                 $VIDEO"

section "health"
curl -sf "$BASE/health" | jq .
pass "health endpoint reachable"

section "status (expect Idle)"
state=$(curl -sf "$BASE/status" | jq -r '.state')
[[ "$state" == "Idle" ]] || fail "expected Idle before play, got $state"
pass "state=Idle"

section "play"
curl -sf -X POST "$BASE/play" \
  -H "Content-Type: application/json" \
  -d "{\"url\":\"$VIDEO\",\"video_id\":\"00000000-0000-0000-0000-000000000001\"}" | jq .
sleep 1

section "status (expect Playing)"
result=$(curl -sf "$BASE/status")
echo "$result" | jq .
state=$(echo "$result" | jq -r '.state')
[[ "$state" == "Playing" ]] || fail "expected Playing after play, got $state"
pass "state=Playing"

section "pause"
curl -sf -X POST "$BASE/pause" | jq .
sleep 1
state=$(curl -sf "$BASE/status" | jq -r '.state')
[[ "$state" == "Paused" ]] || fail "expected Paused after pause, got $state"
pass "state=Paused"

section "resume"
curl -sf -X POST "$BASE/resume" | jq .
sleep 1
state=$(curl -sf "$BASE/status" | jq -r '.state')
[[ "$state" == "Playing" ]] || fail "expected Playing after resume, got $state"
pass "state=Playing after resume"

section "stop"
curl -sf -X POST "$BASE/stop" | jq .
sleep 1
state=$(curl -sf "$BASE/status" | jq -r '.state')
[[ "$state" == "Idle" ]] || fail "expected Idle after stop, got $state"
pass "state=Idle after stop"

printf "\nAll tests passed.\n"
