#!/usr/bin/env bash
# Re-encode a video to something a Pi 5 can actually play.
#
# Usage:
#   ./scripts/transcode.sh videos/*.mp4          # whole library, skips what is fine
#   ./scripts/transcode.sh 'videos/golden times.mp4'
#   MAX_MBPS=6 ./scripts/transcode.sh videos/big.mp4
#   FORCE=1 ./scripts/transcode.sh videos/x.mp4  # re-encode even if under the threshold
#
# Why this exists: the Pi 5 has no H.264 hardware decoder at all (HEVC only),
# so every H.264 frame is decoded on the CPU, and the agent streams the file
# over HTTP from the server for the whole playback. A master export straight
# out of an editor — 1080p H.264 at 65 Mbps was the case that prompted this —
# loses on both counts at once and plays as a slideshow. A delivery-bitrate
# encode fixes it without visible quality loss on a TV.
#
# Originals are never destroyed: each one moves to masters/ (gitignored) and
# the encode takes its place, so `videos/` stays flat and the filename the
# dashboard shows does not change.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MASTERS_DIR="${MASTERS_DIR:-$REPO_DIR/masters}"

# Cap, not a target. -crf drives quality and most scenes land well under this;
# the cap is what stops a busy scene spiking past what the Pi can decode or the
# Wi-Fi can carry. 8 Mbps is a normal high-quality 1080p delivery rate.
MAX_MBPS="${MAX_MBPS:-8}"
# x264's quality knob. 21 is visually transparent at 1080p for this material;
# lower is better and bigger. The bitrate cap above still applies.
CRF="${CRF:-21}"

command -v ffmpeg >/dev/null || { echo "ffmpeg not found (brew install ffmpeg)" >&2; exit 1; }
command -v ffprobe >/dev/null || { echo "ffprobe not found (brew install ffmpeg)" >&2; exit 1; }
[[ $# -gt 0 ]] || { echo "Usage: $0 <video>..." >&2; exit 1; }

# Overall bitrate in bits/sec, from the container. Falls back to size/duration,
# which is what some containers force — ffprobe reports no format-level bitrate
# for them. Prints nothing if neither is available.
bitrate_of() {
  local f="$1" br dur size
  br="$(ffprobe -v error -show_entries format=bit_rate -of csv=p=0 "$f" 2>/dev/null || true)"
  if [[ "$br" =~ ^[0-9]+$ ]]; then printf '%s\n' "$br"; return; fi
  dur="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$f" 2>/dev/null || true)"
  [[ "$dur" =~ ^[0-9.]+$ ]] || return 0
  size="$(wc -c <"$f")"
  awk -v s="$size" -v d="$dur" 'BEGIN { if (d > 0) printf "%d\n", s * 8 / d }'
}

mkdir -p "$MASTERS_DIR"

for input in "$@"; do
  [[ -f "$input" ]] || { echo "!! no such file: $input" >&2; continue; }
  name="$(basename "$input")"

  printf '\n== %s\n' "$name"
  br="$(bitrate_of "$input")"
  if [[ -n "$br" ]]; then
    printf '   current  %.1f Mbps\n' "$(awk -v b="$br" 'BEGIN { print b/1000000 }')"
    # Re-running over the library must be cheap and idempotent, so anything
    # already at a sane rate is left alone. The 1.2 factor keeps a file that is
    # a hair over the cap from being re-encoded for no visible gain.
    if [[ "${FORCE:-}" != "1" ]] \
       && awk -v b="$br" -v m="$MAX_MBPS" 'BEGIN { exit !(b <= m * 1000000 * 1.2) }'; then
      echo "   skip     already within ${MAX_MBPS} Mbps"
      continue
    fi
  else
    echo "   current  unknown — encoding anyway"
  fi

  tmp="$(dirname "$input")/.transcode-$$-$name"
  # Clean up a half-written encode if ffmpeg dies or the run is interrupted;
  # a truncated file left in videos/ would be indexed and served as playable.
  trap 'rm -f "$tmp"' EXIT

  echo "   encoding CRF $CRF, cap ${MAX_MBPS} Mbps"
  ffmpeg -nostdin -v error -stats -i "$input" \
    -c:v libx264 -preset slow -crf "$CRF" \
    -maxrate "${MAX_MBPS}M" -bufsize "$((MAX_MBPS * 2))M" \
    -profile:v high -level 4.0 \
    -pix_fmt yuv420p \
    -c:a aac -b:a 192k -ac 2 \
    -movflags +faststart \
    "$tmp"

  mv -f "$input" "$MASTERS_DIR/$name"
  mv -f "$tmp" "$input"
  trap - EXIT

  new="$(bitrate_of "$input")"
  [[ -n "$new" ]] && printf '   now      %.1f Mbps\n' "$(awk -v b="$new" 'BEGIN { print b/1000000 }')"
  printf '   original %s\n' "$MASTERS_DIR/$name"
done

printf '\nDone. Originals are in %s\n' "$MASTERS_DIR"
