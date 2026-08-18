#!/usr/bin/env bash
# Cross-compile pi-agent and install it on a Pi.
#
# Usage:
#   ./deploy_agent.sh pi@tv-01.local
#   ./deploy_agent.sh pi@192.168.1.11
#   TARGET_ARCH=armv7-unknown-linux-gnueabihf ./deploy_agent.sh pi@tv-05.local
#   SKIP_BUILD=1 ./deploy_agent.sh pi@tv-02.local   # reuse the last build
#
# Deploying to a fleet:
#   for n in 01 02 03; do ./deploy_agent.sh pi@tv-$n.local; done
# (the first run builds, the rest reuse the artifact)
#
# Requires `cross` (cargo install cross) and a running Docker, or set
# USE_CARGO=1 if your host already has the target's linker installed.
#
# The install step runs sudo over a non-interactive ssh session, so the account
# needs passwordless sudo — the default on Raspberry Pi OS. If yours prompts,
# add a NOPASSWD rule or install the binary by hand.

set -euo pipefail

TARGET_HOST="${1:-${TARGET_HOST:-}}"
# Pi 5 / Pi 4 / Pi 3 on a 64-bit OS. Override for 32-bit images — see
# Cross.toml for the targets that are set up.
TARGET_ARCH="${TARGET_ARCH:-aarch64-unknown-linux-gnu}"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$REPO_DIR/target/$TARGET_ARCH/release/pi-agent"

if [[ -z "$TARGET_HOST" ]]; then
  echo "Usage: $0 user@host   (e.g. $0 pi@tv-01.local)" >&2
  exit 1
fi

step() { printf "\n== %s\n" "$1"; }

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  step "Building pi-agent for $TARGET_ARCH"
  if [[ "${USE_CARGO:-}" == "1" ]]; then
    (cd "$REPO_DIR" && cargo build --release -p pi-agent --target "$TARGET_ARCH")
  else
    command -v cross >/dev/null || {
      echo "cross not found. Install it with 'cargo install cross', or set USE_CARGO=1." >&2
      exit 1
    }
    (cd "$REPO_DIR" && cross build --release -p pi-agent --target "$TARGET_ARCH")
  fi
fi

[[ -f "$BINARY" ]] || { echo "No binary at $BINARY" >&2; exit 1; }

step "Checking $TARGET_HOST"
# Fail here with a clear message rather than midway through the install.
ssh -o BatchMode=yes -o ConnectTimeout=10 "$TARGET_HOST" \
  "test -d /etc/tv-agent" \
  || { echo "Cannot reach $TARGET_HOST, or /etc/tv-agent is missing (run setup_pi.sh there first)." >&2; exit 1; }

step "Copying $(basename "$BINARY") ($(du -h "$BINARY" | cut -f1))"
# Staged in /tmp because the running binary cannot be overwritten in place.
scp "$BINARY" "$TARGET_HOST:/tmp/pi-agent.new"

step "Installing and restarting"
ssh "$TARGET_HOST" 'bash -se' <<'REMOTE'
set -euo pipefail
sudo install -m 755 /tmp/pi-agent.new /usr/local/bin/pi-agent
rm -f /tmp/pi-agent.new
if systemctl list-unit-files tv-agent.service >/dev/null 2>&1; then
  sudo systemctl restart tv-agent
  sleep 2
  systemctl is-active --quiet tv-agent \
    && echo "tv-agent is running" \
    || { echo "tv-agent failed to start:"; sudo journalctl -u tv-agent -n 20 --no-pager; exit 1; }
else
  echo "tv-agent.service not installed — run setup_pi.sh, then: sudo systemctl enable --now tv-agent"
fi
REMOTE

step "Health check"
HOST_ONLY="${TARGET_HOST#*@}"
if curl -fsS --max-time 5 "http://$HOST_ONLY:8080/health" 2>/dev/null; then
  printf "\nDeployed to %s\n" "$TARGET_HOST"
else
  printf "\nBinary installed, but http://%s:8080/health did not answer.\n" "$HOST_ONLY"
  echo "Check: ssh $TARGET_HOST 'journalctl -u tv-agent -n 50 --no-pager'"
  exit 1
fi
