#!/usr/bin/env bash
# Re-encode a video to something the target Pi can actually play.
#
# Usage:
#   ./scripts/transcode.sh videos/*.mp4          # whole library, skips what is fine
#   ./scripts/transcode.sh 'videos/golden times.mp4'
#   MAX_MBPS=6 ./scripts/transcode.sh videos/big.mp4
#   PROFILE=pi3 ./scripts/transcode.sh videos/*.mp4   # 720p, for a Pi 3 / Zero 2
#   FORCE=1 ./scripts/transcode.sh videos/x.mp4  # re-encode even if already within spec
#
# Why this exists: the agent streams the file over HTTP from the server for the
# whole playback, and the Pi then has to decode it. A master export straight out
# of an editor — 1080p H.264 at 65 Mbps was the case that prompted this — loses
# on both counts at once and plays as a slideshow.
#
# There are two independent limits, and which one binds depends on the hardware:
#
#   Bitrate  — what the network carries and, on a Pi 5, what the CPU decodes.
#              A Pi 5 has no H.264 hardware decoder at all (HEVC only), so every
#              frame is decoded on four fast A76 cores. 1080p at ~8 Mbps is fine.
#
#   Resolution — what a Pi 3 / Zero 2 W can decode at all. Its VideoCore IV
#              *does* have an H.264 decoder, but it needs mpv pointed at it
#              (see scripts/setup_pi.sh); with software decode on four 1.2 GHz
#              A53 cores, 1080p does not play at any bitrate. Lowering Mbps
#              alone does nothing here — the pixel rate is the problem, so the
#              fix is 720p. That is what PROFILE=pi3 selects.
#
# Originals are never destroyed: each one moves to masters/ (gitignored) and
# the encode takes its place, so `videos/` stays flat and the filename the
# dashboard shows does not change.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MASTERS_DIR="${MASTERS_DIR:-$REPO_DIR/masters}"

# PROFILE picks defaults for a class of hardware; the individual knobs below
# still override it, so PROFILE=pi3 MAX_MBPS=3 is a valid combination.
PROFILE="${PROFILE:-default}"
case "$PROFILE" in
  # A Pi 3 decodes 720p comfortably and 1080p not at all. 4 Mbps is a normal
  # 720p delivery rate, and it also halves what the Pi 3's 100 Mbit NIC — which
  # shares the USB 2.0 bus with everything else — has to carry.
  pi3)     PROFILE_MAX_MBPS=4; PROFILE_MAX_HEIGHT=720;  PROFILE_CRF=23 ;;
  default) PROFILE_MAX_MBPS=8; PROFILE_MAX_HEIGHT=1080; PROFILE_CRF=21 ;;
  *) echo "PROFILE must be one of: default, pi3" >&2; exit 1 ;;
esac

# Cap, not a target. -crf drives quality and most scenes land well under this;
# the cap is what stops a busy scene spiking past what the Pi can decode or the
# Wi-Fi can carry. 8 Mbps is a normal high-quality 1080p delivery rate.
MAX_MBPS="${MAX_MBPS:-$PROFILE_MAX_MBPS}"
# Downscale anything bigger than this, expressed as the height of a 16:9 frame
# because that is how everyone says it ("720p"). What is actually enforced is
# the pixel count that implies, so portrait and other aspect ratios are judged
# on the work they cost rather than on their height. Unlike the bitrate cap,
# this is the limit that decides whether a small Pi can decode a file at all.
MAX_HEIGHT="${MAX_HEIGHT:-$PROFILE_MAX_HEIGHT}"
PIXEL_BUDGET=$(( MAX_HEIGHT * MAX_HEIGHT * 16 / 9 ))
# x264's quality knob. 21 is visually transparent at 1080p for this material;
# lower is better and bigger. The bitrate cap above still applies.
CRF="${CRF:-$PROFILE_CRF}"

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

# "W H" of the first video stream. Prints nothing if unreadable.
dims_of() {
  local d w h
  d="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height \
       -of csv=p=0 "$1" 2>/dev/null | head -1 || true)"
  w="${d%%,*}"; h="${d##*,}"
  [[ "$w" =~ ^[0-9]+$ && "$h" =~ ^[0-9]+$ ]] && printf '%s %s\n' "$w" "$h"
}

# Dimensions that fit inside the pixel budget, preserving aspect ratio.
# Prints "W H", or nothing at all when the source already fits.
#
# The budget is on total pixels, not on height. A decoder's limit is a pixel
# rate, and this library is part portrait: 1080x1350 vertical signage has 30%
# FEWER pixels than 1920x1080, so a height limit would downscale the cheaper
# file while passing the more expensive one.
scaled_dims() {
  awk -v w="$1" -v h="$2" -v budget="$3" 'BEGIN {
    px = w * h
    if (px <= budget) exit 0
    f = sqrt(budget / px)
    # Round to even: H.264 4:2:0 chroma subsampling requires even dimensions.
    nw = int(w * f / 2 + 0.5) * 2
    nh = int(h * f / 2 + 0.5) * 2
    if (nw < 2) nw = 2
    if (nh < 2) nh = 2
    printf "%d %d\n", nw, nh
  }'
}

mkdir -p "$MASTERS_DIR"

for input in "$@"; do
  [[ -f "$input" ]] || { echo "!! no such file: $input" >&2; continue; }
  name="$(basename "$input")"

  printf '\n== %s\n' "$name"
  br="$(bitrate_of "$input")"
  read -r width height <<<"$(dims_of "$input")"
  dims_label="${width:-?}x${height:-?}"
  if [[ -n "$br" ]]; then
    printf '   current  %.1f Mbps, %s\n' \
      "$(awk -v b="$br" 'BEGIN { print b/1000000 }')" "$dims_label"
  else
    echo "   current  bitrate unknown, $dims_label"
  fi

  # Re-running over the library must be cheap and idempotent, so a file already
  # within spec is left alone. Both limits have to pass: a 1080p file at 5 Mbps
  # is under the bitrate cap but still unplayable on a Pi 3, and skipping on
  # bitrate alone would silently leave the whole library at 1080p.
  #
  # The 1.2 factor keeps a file a hair over the cap from being re-encoded for
  # no visible gain. Height gets no such slack — it is a hard decoder limit.
  # scaled_dims prints nothing when the source already fits the budget, so an
  # empty result *is* the "within size" answer.
  SCALE_ARGS=()
  within_size=0
  if [[ -n "$width" && -n "$height" ]]; then
    read -r new_w new_h <<<"$(scaled_dims "$width" "$height" "$PIXEL_BUDGET")"
    if [[ -z "${new_w:-}" ]]; then
      within_size=1
    else
      SCALE_ARGS=(-vf "scale=$new_w:$new_h")
    fi
  fi

  within_bitrate=0
  [[ -n "$br" ]] \
    && awk -v b="$br" -v m="$MAX_MBPS" 'BEGIN { exit !(b <= m * 1000000 * 1.2) }' \
    && within_bitrate=1

  if [[ "${FORCE:-}" != "1" ]] && (( within_bitrate && within_size )); then
    echo "   skip     already within ${MAX_MBPS} Mbps and ${MAX_HEIGHT}p-equivalent"
    continue
  fi
  # Unreadable bitrate or dimensions leave the corresponding flag at 0, so the
  # file is encoded rather than assumed fine.

  [[ ${#SCALE_ARGS[@]} -gt 0 ]] \
    && echo "   scaling  ${width}x${height} -> ${new_w}x${new_h}"

  tmp="$(dirname "$input")/.transcode-$$-$name"
  # Clean up a half-written encode if ffmpeg dies or the run is interrupted;
  # a truncated file left in videos/ would be indexed and served as playable.
  trap 'rm -f "$tmp"' EXIT

  # Always encode from the most original copy available. Re-running with a
  # different PROFILE would otherwise re-encode the previous *encode* — a
  # second generation of loss for no reason — and, worse, the mv below would
  # overwrite the real master with that intermediate and lose the original for
  # good. Whichever profile is run, the file in masters/ stays the first one.
  source="$input"
  if [[ -f "$MASTERS_DIR/$name" ]]; then
    source="$MASTERS_DIR/$name"
    echo "   source   masters/$name (re-encoding from the original)"
  fi

  echo "   encoding CRF $CRF, cap ${MAX_MBPS} Mbps"
  ffmpeg -nostdin -v error -stats -i "$source" \
    ${SCALE_ARGS[@]+"${SCALE_ARGS[@]}"} \
    -c:v libx264 -preset slow -crf "$CRF" \
    -maxrate "${MAX_MBPS}M" -bufsize "$((MAX_MBPS * 2))M" \
    -profile:v high -level 4.0 \
    -pix_fmt yuv420p \
    -r 25 \
    -c:a aac -b:a 128k -ac 2 \
    -movflags +faststart \
    "$tmp"

  if [[ -f "$MASTERS_DIR/$name" ]]; then
    rm -f "$input"          # the master is already safe; this was an encode
  else
    mv -f "$input" "$MASTERS_DIR/$name"
  fi
  mv -f "$tmp" "$input"
  trap - EXIT

  new="$(bitrate_of "$input")"
  read -r out_w out_h <<<"$(dims_of "$input")"
  [[ -n "$new" ]] && printf '   now      %.1f Mbps, %sx%s\n' \
    "$(awk -v b="$new" 'BEGIN { print b/1000000 }')" "${out_w:-?}" "${out_h:-?}"
  printf '   original %s\n' "$MASTERS_DIR/$name"
done

printf '\nDone. Originals are in %s\n' "$MASTERS_DIR"
