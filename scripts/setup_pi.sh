#!/usr/bin/env bash
# One-time Pi provisioning, run on a fresh Raspberry Pi OS install.
# Produces the state you then `dd` off the card as the golden image.
#
# Installs mpv, creates /etc/tv-agent, writes an mpv config that can actually
# reach the screen, and installs the systemd unit. It does NOT install the
# pi-agent binary — that is deploy_agent.sh, so the golden image can be cloned
# before the binary is final.
#
# Usage:
#   sudo ./setup_pi.sh                 # run user defaults to pi
#   sudo RUN_USER=tv ./setup_pi.sh
#   sudo SESSION=lite ./setup_pi.sh    # force the display mode, skipping detection
#
# SESSION is auto-detected (lite | wayland | x11) and decides both the mpv
# output configuration and the display variable in the systemd unit. Override
# it when provisioning a card for a machine other than the one you are on.
#
# After running:
#   1. edit /etc/tv-agent/.env  (SERVER_URL, DEVICE_NAME)
#   2. ./scripts/deploy_agent.sh pi@this-host   from your workstation
#   3. sudo systemctl enable --now tv-agent
#   4. shut down and image the card

set -euo pipefail

RUN_USER="${RUN_USER:-pi}"
CONF_DIR=/etc/tv-agent
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() { printf "\n== %s\n" "$1"; }
warn() { printf "[WARN] %s\n" "$1" >&2; }

if [[ $EUID -ne 0 ]]; then
  echo "Run with sudo: sudo $0" >&2
  exit 1
fi

if ! id "$RUN_USER" &>/dev/null; then
  echo "User '$RUN_USER' does not exist. Create it first, or set RUN_USER." >&2
  exit 1
fi

step "Installing packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update
# mpv is the whole runtime dependency. ffprobe (in ffmpeg) is only used by the
# server to read video durations, so it is deliberately not installed here.
apt-get install -y mpv ca-certificates

step "Granting $RUN_USER access to the display hardware"
# On Pi OS Lite there is no X or Wayland session: mpv drives KMS/DRM directly,
# which needs these groups. Harmless on a desktop install.
for group in video render audio; do
  if getent group "$group" >/dev/null; then
    usermod -aG "$group" "$RUN_USER"
  fi
done

step "Creating $CONF_DIR"
# The agent generates and persists its device UUID here on first start, so the
# directory has to be writable by the user the service runs as.
mkdir -p "$CONF_DIR"
chown "$RUN_USER":"$RUN_USER" "$CONF_DIR"
chmod 755 "$CONF_DIR"

if [[ -f "$CONF_DIR/.env" ]]; then
  echo "$CONF_DIR/.env already exists — leaving it alone"
else
  install -o "$RUN_USER" -g "$RUN_USER" -m 644 \
    "$REPO_DIR/pi-agent/deploy/env.example" "$CONF_DIR/.env"
  warn "Edit $CONF_DIR/.env — SERVER_URL and DEVICE_NAME are placeholders"
fi

step "Detecting the display session"
# Which display the agent's mpv should render to is the single most common
# thing to get wrong, and it fails silently: mpv accepts every command and
# plays to nothing. Detect it rather than leaving it to a commented-out line.
RUN_UID="$(id -u "$RUN_USER")"

detect_session() {
  local uid="$1" sock x
  # A live Wayland compositor leaves a socket here. The name is not always
  # wayland-0 — a restart or a second compositor gives wayland-1.
  for sock in /run/user/"$uid"/wayland-*; do
    [[ -S "$sock" ]] || continue          # skips the .lock file
    printf 'wayland:%s\n' "${sock##*/}"
    return
  done
  # X11: /tmp/.X11-unix/X<n> means DISPLAY=:<n>
  for x in /tmp/.X11-unix/X*; do
    [[ -S "$x" ]] || continue
    printf 'x11::%s\n' "${x##*/X}"
    return
  done
  printf 'lite:\n'
}

SESSION="${SESSION:-auto}"
if [[ "$SESSION" == "auto" ]]; then
  detected="$(detect_session "$RUN_UID")"
  SESSION_KIND="${detected%%:*}"
  SESSION_ADDR="${detected#*:}"
  if [[ "$SESSION_KIND" == "lite" ]] \
     && [[ "$(systemctl get-default 2>/dev/null)" == "graphical.target" ]]; then
    warn "No display session found, but the default target is graphical.target."
    warn "If this is a desktop install, start the session and re-run, or pass"
    warn "SESSION=wayland (or SESSION=x11) — otherwise mpv will render to nothing."
  fi
else
  SESSION_KIND="$SESSION"
  case "$SESSION_KIND" in
    wayland) SESSION_ADDR="${SESSION_ADDR:-wayland-0}" ;;
    x11)     SESSION_ADDR="${SESSION_ADDR:-:0}" ;;
    lite)    SESSION_ADDR="" ;;
    *) echo "SESSION must be one of: auto, lite, wayland, x11" >&2; exit 1 ;;
  esac
fi

case "$SESSION_KIND" in
  wayland) echo "Wayland session, WAYLAND_DISPLAY=$SESSION_ADDR" ;;
  x11)     echo "X11 session, DISPLAY=$SESSION_ADDR" ;;
  lite)    echo "No desktop session — mpv will drive KMS/DRM directly" ;;
esac

step "Configuring mpv output"
# The agent spawns a bare `mpv --input-ipc-server=... --idle=yes --no-terminal`
# with no video-output flags, so output is configured here rather than in code.
# Without this, mpv on a headless Pi accepts commands and plays nothing.
USER_HOME="$(getent passwd "$RUN_USER" | cut -d: -f6)"
MPV_CONF_DIR="$USER_HOME/.config/mpv"
mkdir -p "$MPV_CONF_DIR"
if [[ -f "$MPV_CONF_DIR/mpv.conf" ]]; then
  echo "$MPV_CONF_DIR/mpv.conf already exists — leaving it alone"
  # A Lite-shaped config on a desktop install is the silent-playback trap: the
  # compositor already holds DRM, so mpv cannot get it and renders nowhere.
  if [[ "$SESSION_KIND" != "lite" ]] \
     && grep -qE '^[[:space:]]*gpu-context=drm' "$MPV_CONF_DIR/mpv.conf"; then
    warn "That file sets gpu-context=drm, but a $SESSION_KIND session is running."
    warn "Comment out vo=gpu and gpu-context=drm, or playback will render to nothing."
  fi
else
  {
    echo "# Written by scripts/setup_pi.sh for a $SESSION_KIND session."
    echo "#"
    if [[ "$SESSION_KIND" == "lite" ]]; then
      echo "# No desktop session: render straight to the display via KMS/DRM."
      echo "# If you install a desktop later, comment these two out."
      echo "vo=gpu"
      echo "gpu-context=drm"
    else
      echo "# A $SESSION_KIND session is running, so mpv renders into it and picks"
      echo "# its own output. Forcing vo/gpu-context here would fight the compositor."
    fi
    echo ""
    echo "fullscreen=yes"
    echo "# A signage screen should not show mpv's overlay or stop at end of file."
    echo "osc=no"
    echo "osd-level=0"
    echo "keep-open=yes"
  } > "$MPV_CONF_DIR/mpv.conf"
  echo "Wrote $MPV_CONF_DIR/mpv.conf for a $SESSION_KIND session"
fi
chown -R "$RUN_USER":"$RUN_USER" "$USER_HOME/.config"

# The agent hands mpv an HTTP URL on the server, so every frame arrives over
# the LAN for the whole playback. mpv's defaults read barely ahead of the
# picture, so a brief Wi-Fi stall shows up as a frozen frame and a dropped
# connection never recovers on its own.
#
# This is appended rather than folded into the block above, because that block
# leaves an existing mpv.conf alone — a Pi provisioned before this tuning
# existed would otherwise never get it.
#
# The block is delimited at both ends and REPLACED on a re-run, not skipped.
# It used to be skipped whenever the start marker was found, which meant a Pi
# provisioned before a tuning change could never receive the corrected values —
# exactly the case that matters, since the values below depend on which Pi this
# is and the first version of this block assumed a Pi 5.
MPV_TUNING_START='# --- tv-controller streaming tuning ---'
MPV_TUNING_END='# --- end tv-controller streaming tuning ---'

step "Detecting the video hardware"
# Which decoder mpv should use is a per-model decision, and getting it wrong
# costs the whole picture rather than a few percent:
#
#   Pi 3 / Zero 2 W: VideoCore IV *does* have an H.264 decoder (1080p30, High
#     profile level 4.0). On current Pi OS it is reached through V4L2 M2M on
#     /dev/video10, and mpv's `auto-safe` does NOT select that — the safe list
#     is deliberately conservative. So `hwdec=auto-safe` on a Pi 3 silently
#     software-decodes on four 1.2 GHz A53 cores, which cannot sustain 1080p
#     at any bitrate. It has to be named explicitly.
#   Pi 4: same V4L2 path, faster cores.
#   Pi 5: no H.264 block at all (HEVC only). Software decode is the only
#     option and the A76 cores are fast enough for 1080p at delivery rates.
#
# So probe for the decoder device rather than hardcoding a model.
PI_MODEL="$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || echo 'unknown')"
TOTAL_RAM_MB="$(awk '/^MemTotal:/ { print int($2 / 1024) }' /proc/meminfo)"
echo "Model:  $PI_MODEL"
echo "Memory: ${TOTAL_RAM_MB} MB"

# /dev/video10 is the bcm2835-codec stateful decoder, present on Pi 3 and Pi 4
# and absent on Pi 5. Its presence is the thing that decides this, not the
# model string, which varies in wording across OS releases.
if [[ -c /dev/video10 ]]; then
  HWDEC="v4l2m2m-copy"
  HWDEC_WHY="VideoCore H.264 decoder present (/dev/video10); auto-safe would not use it"
else
  HWDEC="auto-safe"
  HWDEC_WHY="no V4L2 H.264 decoder — software decode (correct on a Pi 5)"
fi
echo "hwdec:  $HWDEC — $HWDEC_WHY"

# Read-ahead has to be sized against RAM, not picked once. 200MiB of demuxer
# cache is a rounding error on an 8 GB Pi 5 and a fifth of total memory on a
# 1 GB Pi 3 — where it competes with the GPU carve-out and pushes the box into
# swap on the SD card, which stutters far worse than the network stall the
# cache exists to absorb.
if   (( TOTAL_RAM_MB <= 1280 )); then CACHE_SECS=10; DEMUX_MAX="32MiB"
elif (( TOTAL_RAM_MB <= 3072 )); then CACHE_SECS=20; DEMUX_MAX="96MiB"
else                                  CACHE_SECS=30; DEMUX_MAX="200MiB"
fi
echo "cache:  ${CACHE_SECS}s / ${DEMUX_MAX} for ${TOTAL_RAM_MB} MB of RAM"

step "Tuning mpv for streaming playback"
# Strip any previous block before re-appending, so the values above always win.
# awk on a fixed-string match rather than `sed -i` with an escaped pattern:
# the markers are literals, and this needs no GNU-specific -i either.
#
# `end == ""` handles the original form of this block, which had no end marker
# and always ran to the end of the file.
if [[ -f "$MPV_CONF_DIR/mpv.conf" ]] \
   && grep -qF "$MPV_TUNING_START" "$MPV_CONF_DIR/mpv.conf"; then
  # Also trims trailing blank lines, so repeated runs converge on one file
  # instead of growing a blank line per run.
  awk -v start="$MPV_TUNING_START" -v end="$MPV_TUNING_END" '
    $0 == start { skipping = 1; next }
    skipping && end != "" && $0 == end { skipping = 0; next }
    skipping { next }
    { lines[++n] = $0; if (NF) last = n }
    END { for (i = 1; i <= last; i++) print lines[i] }
  ' "$MPV_CONF_DIR/mpv.conf" > "$MPV_CONF_DIR/mpv.conf.tmp"
  mv "$MPV_CONF_DIR/mpv.conf.tmp" "$MPV_CONF_DIR/mpv.conf"
  echo "Replaced the existing tuning block"
fi

cat >> "$MPV_CONF_DIR/mpv.conf" <<MPVTUNING

$MPV_TUNING_START
# Written by scripts/setup_pi.sh for: $PI_MODEL (${TOTAL_RAM_MB} MB)
# Read ahead, so a short network stall drains the buffer instead of the screen.
# Sized against this machine's RAM — an oversized cache on a small Pi swaps to
# the SD card, which is worse than the stall it was meant to hide.
cache=yes
cache-secs=$CACHE_SECS
demuxer-max-bytes=$DEMUX_MAX
# Reconnect rather than sitting on a dead HTTP connection to the server.
stream-lavf-o=reconnect=1,reconnect_streamed=1,reconnect_delay_max=5
# $HWDEC_WHY
hwdec=$HWDEC
$MPV_TUNING_END
MPVTUNING
echo "Wrote streaming tuning to $MPV_CONF_DIR/mpv.conf"

# Appending leaves ownership alone, but be explicit in case the file was new.
chown "$RUN_USER":"$RUN_USER" "$MPV_CONF_DIR/mpv.conf"

step "Installing the systemd unit"
# The shipped unit is written for the default `pi` account; User, HOME and
# XDG_RUNTIME_DIR all have to agree with whoever actually runs it.
# The shipped unit ships both display lines commented out; uncomment whichever
# matches the session detected above, and leave the other alone.
SED_ARGS=(
  -e "s|^User=.*|User=$RUN_USER|"
  -e "s|^Environment=HOME=.*|Environment=HOME=$USER_HOME|"
  -e "s|^Environment=XDG_RUNTIME_DIR=.*|Environment=XDG_RUNTIME_DIR=/run/user/$RUN_UID|"
)
case "$SESSION_KIND" in
  wayland) SED_ARGS+=(-e "s|^#\?Environment=WAYLAND_DISPLAY=.*|Environment=WAYLAND_DISPLAY=$SESSION_ADDR|") ;;
  x11)     SED_ARGS+=(-e "s|^#\?Environment=DISPLAY=.*|Environment=DISPLAY=$SESSION_ADDR|") ;;
esac
sed "${SED_ARGS[@]}" \
    "$REPO_DIR/pi-agent/deploy/tv-agent.service" \
  > /etc/systemd/system/tv-agent.service
# Synchronised playback across several TVs compares wall-clock time on each
# Pi, so the fleet has to agree on it. Pi OS ships systemd-timesyncd and
# usually has it on already; make it explicit rather than assumed, because the
# failure mode is silent — playback simply never lines up, with nothing in the
# logs to say why.
if command -v timedatectl >/dev/null 2>&1; then
  timedatectl set-ntp true 2>/dev/null \
    && echo "Enabled NTP time sync (required for synchronised playback)" \
    || echo "WARNING: could not enable NTP; synchronised playback needs a correct clock"
else
  echo "WARNING: timedatectl not found; ensure this Pi's clock is NTP-synced"
fi

systemctl daemon-reload
echo "Installed /etc/systemd/system/tv-agent.service (not enabled yet)"

step "Disabling screen blanking"
# consoleblank only applies to the Lite console; ignore failures elsewhere.
if [[ -f /boot/firmware/cmdline.txt ]] && ! grep -q consoleblank /boot/firmware/cmdline.txt; then
  sed -i 's/$/ consoleblank=0/' /boot/firmware/cmdline.txt
  echo "Added consoleblank=0 (takes effect on reboot)"
fi

cat <<DONE

Done. Next:
  1. edit $CONF_DIR/.env            (SERVER_URL, DEVICE_NAME)
  2. deploy the binary from your workstation:
       ./scripts/deploy_agent.sh $RUN_USER@\$(hostname)
  3. sudo systemctl enable --now tv-agent
  4. curl localhost:8080/health
  5. sudo shutdown -h now, then image the card

Note: group membership (video/render) only applies after a reboot.
Display: $SESSION_KIND${SESSION_ADDR:+ ($SESSION_ADDR)} — re-run with SESSION=... to change.
DONE
