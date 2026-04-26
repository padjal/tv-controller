#!/usr/bin/env bash
# Trigger playback of a file or URL on the pi-agent
# Requires: curl, jq
#
# Usage:
#   PI_HOST=192.168.1.11 ./play.sh /tmp/test.mp4
#   ./play.sh 192.168.1.11 /tmp/test.mp4
#   ./play.sh 192.168.1.11 http://server:8000/videos/movie.mp4

set -euo pipefail

PI_HOST="${1:-${PI_HOST:-192.168.1.11}}"
PI_PORT="${PI_PORT:-8080}"
VIDEO="${2:-/tmp/test.mp4}"
BASE="http://${PI_HOST}:${PI_PORT}"

curl -sf -X POST "$BASE/play" \
  -H "Content-Type: application/json" \
  -d "{\"url\":\"$VIDEO\",\"video_id\":\"00000000-0000-0000-0000-000000000001\"}" | jq .
