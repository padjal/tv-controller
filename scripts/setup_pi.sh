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

step "Configuring mpv output"
# The agent spawns a bare `mpv --input-ipc-server=... --idle=yes --no-terminal`
# with no video-output flags, so output is configured here rather than in code.
# Without this, mpv on a headless Pi accepts commands and plays nothing.
USER_HOME="$(getent passwd "$RUN_USER" | cut -d: -f6)"
MPV_CONF_DIR="$USER_HOME/.config/mpv"
mkdir -p "$MPV_CONF_DIR"
if [[ -f "$MPV_CONF_DIR/mpv.conf" ]]; then
  echo "$MPV_CONF_DIR/mpv.conf already exists — leaving it alone"
else
  cat > "$MPV_CONF_DIR/mpv.conf" <<'MPVCONF'
# Written by scripts/setup_pi.sh.
#
# Pi OS Lite (no desktop): render straight to the display via KMS/DRM.
# On a desktop install, comment the next two lines out — the session's
# DISPLAY/WAYLAND_DISPLAY (set in the systemd unit) takes over.
vo=gpu
gpu-context=drm

fullscreen=yes
# A signage screen should not show mpv's overlay or stop at end of file.
osc=no
osd-level=0
keep-open=yes
MPVCONF
fi
chown -R "$RUN_USER":"$RUN_USER" "$USER_HOME/.config"

step "Installing the systemd unit"
# The shipped unit is written for the default `pi` account; User, HOME and
# XDG_RUNTIME_DIR all have to agree with whoever actually runs it.
RUN_UID="$(id -u "$RUN_USER")"
sed -e "s|^User=.*|User=$RUN_USER|" \
    -e "s|^Environment=HOME=.*|Environment=HOME=$USER_HOME|" \
    -e "s|^Environment=XDG_RUNTIME_DIR=.*|Environment=XDG_RUNTIME_DIR=/run/user/$RUN_UID|" \
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
DONE
