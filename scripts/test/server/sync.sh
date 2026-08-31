#!/usr/bin/env bash
# Measure how tightly the TVs agree while playing the same video.
# Requires: curl, jq
#
# This is the check that cannot be written as a unit test: it asks every agent
# where its playhead actually is, at the same moment, and reports the spread.
#
# Two runs matter, and they catch different bugs:
#
#   1. Immediately after a multi-TV play — proves the scheduled start worked.
#   2. After an hour of looping — proves drift correction is holding. Each Pi
#      loops on its own crystal, so a fleet that starts together still wanders
#      apart without correction, and only time reveals it.
#
# Positions come from each agent directly rather than from the server, which
# only polls every 10s and would blur exactly what is being measured.
#
# Usage:
#   ./sync.sh                       # every registered device
#   SERVER_HOST=192.168.1.10 ./sync.sh
#   ./sync.sh 192.168.1.10

set -euo pipefail

SERVER_HOST="${1:-${SERVER_HOST:-127.0.0.1}}"
SERVER_PORT="${SERVER_PORT:-8000}"
AGENT_PORT="${AGENT_PORT:-8080}"
BASE="http://${SERVER_HOST}:${SERVER_PORT}/api"

# What counts as "looks simultaneous". Well under the ~100ms a viewer standing
# in front of two screens can pick out.
TOLERANCE="${TOLERANCE:-0.15}"

command -v jq >/dev/null || { echo "jq is required"; exit 1; }

devices="$(curl -sf "$BASE/devices" | jq -c '[.[] | select(.state != "Offline")]')"
count="$(jq 'length' <<<"$devices")"

if [[ "$count" -lt 2 ]]; then
  echo "Need at least two online devices to measure sync; found $count."
  exit 1
fi

echo "Polling $count devices..."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Fired in parallel: polling in sequence would add each request's latency to
# the measurement and manufacture the very skew being measured.
while read -r ip name; do
  (
    pos="$(curl -sf --max-time 3 "http://${ip}:${AGENT_PORT}/status" | jq -r '.position_secs // "null"')"
    printf '%s\t%s\n' "$pos" "$name" > "$tmp/$name.pos"
  ) &
done < <(jq -r '.[] | "\(.ip) \(.name)"' <<<"$devices")
wait

positions=()
printf "\n%-24s %s\n" "TV" "position"
for file in "$tmp"/*.pos; do
  [[ -e "$file" ]] || continue
  IFS=$'\t' read -r pos name < "$file"
  printf "%-24s %s\n" "$name" "$pos"
  [[ "$pos" != "null" && "$pos" != "" ]] && positions+=("$pos")
done

if [[ "${#positions[@]}" -lt 2 ]]; then
  echo
  echo "[FAIL] Fewer than two TVs reported a position — are they playing a video?"
  exit 1
fi

spread="$(printf '%s\n' "${positions[@]}" | sort -n | awk 'NR==1{min=$1} {max=$1} END{printf "%.3f", max-min}')"

echo
echo "Spread: ${spread}s across ${#positions[@]} TVs (tolerance ${TOLERANCE}s)"

if awk "BEGIN{exit !($spread <= $TOLERANCE)}"; then
  echo "[PASS] The TVs are in sync."
else
  echo "[FAIL] The TVs are out of sync by ${spread}s."
  echo "       Check that every Pi has a synced clock: 'timedatectl status' should"
  echo "       report 'System clock synchronized: yes' on each one."
  exit 1
fi
