#!/usr/bin/env bash
# Watch the server's SSE stream. Prints each event as it arrives.
# Requires: curl, jq
#
# Run this in one terminal, then drive the server from another (register a
# device, run playback.sh, drop a video into VIDEOS_DIR) and watch the events.
#
# Usage:
#   SERVER_HOST=192.168.1.10 ./events.sh
#   ./events.sh 192.168.1.10          # Ctrl-C to stop

set -euo pipefail

SERVER_HOST="${1:-${SERVER_HOST:-127.0.0.1}}"
SERVER_PORT="${SERVER_PORT:-8000}"
BASE="http://${SERVER_HOST}:${SERVER_PORT}"

echo "watching $BASE/api/events — Ctrl-C to stop"

# -N disables buffering so frames appear as they arrive. Each SSE frame is
# "data: {...}"; anything else (keep-alive comments, named events) is passed
# through as-is.
curl -sN "$BASE/api/events" | while IFS= read -r line; do
  case "$line" in
    data:*)
      payload="${line#data:}"
      kind=$(printf '%s' "$payload" | jq -r '.kind' 2>/dev/null || echo "?")
      printf '[%s] %s\n' "$kind" "$(printf '%s' "$payload" | jq -c '.payload' 2>/dev/null || printf '%s' "$payload")"
      ;;
    event:*)
      printf '<< %s >>\n' "$line"
      ;;
    "") ;;
    *) printf '%s\n' "$line" ;;
  esac
done
