#!/usr/bin/env bash
# Show current pi-agent status
# Requires: curl, jq
#
# Usage:
#   PI_HOST=192.168.1.11 ./status.sh
#   ./status.sh 192.168.1.11

set -euo pipefail

PI_HOST="${1:-${PI_HOST:-192.168.1.11}}"
PI_PORT="${PI_PORT:-8080}"
BASE="http://${PI_HOST}:${PI_PORT}"

curl -sf "$BASE/status" | jq .
