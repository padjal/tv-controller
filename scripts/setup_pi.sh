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

step "Tuning mpv for streaming playback"
# The agent hands mpv an HTTP URL on the server, so every frame arrives over
# the LAN for the whole playback. mpv's defaults read barely ahead of the
# picture, so a brief Wi-Fi stall shows up as a frozen frame and a dropped
# connection never recovers on its own.
#
# Appended rather than folded into the block above, because that block leaves
# an existing mpv.conf alone — a Pi provisioned before this tuning existed
# would otherwise never get it. The marker keeps a re-run from adding it twice.
MPV_TUNING_MARKER='# --- tv-controller streaming tuning ---'
if grep -qF "$MPV_TUNING_MARKER" "$MPV_CONF_DIR/mpv.conf" 2>/dev/null; then
  echo "Streaming tuning already present in $MPV_CONF_DIR/mpv.conf"
else
  cat >> "$MPV_CONF_DIR/mpv.conf" <<MPVTUNING

$MPV_TUNING_MARKER
# Read ahead, so a short network stall drains the buffer instead of the screen.
cache=yes
cache-secs=30
demuxer-max-bytes=200MiB
# Reconnect rather than sitting on a dead HTTP connection to the server.
stream-lavf-o=reconnect=1,reconnect_streamed=1,reconnect_delay_max=5
# Use a hardware decoder where there is one to use. A Pi 5 has no H.264 block
# at all (HEVC only), so high-bitrate H.264 still decodes on the CPU — if it
# still stutters with this set, the file's bitrate is the problem.
hwdec=auto-safe
MPVTUNING
  echo "Appended streaming tuning to $MPV_CONF_DIR/mpv.conf"
fi
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
