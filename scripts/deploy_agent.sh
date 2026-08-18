#!/usr/bin/env bash
# Cross-compile pi-agent and install it on a Pi.
#
# Usage:
#   ./deploy_agent.sh pi@tv-01.local
#   ./deploy_agent.sh pi@192.168.1.11
#   TARGET_ARCH=armv7-unknown-linux-gnueabihf ./deploy_agent.sh pi@tv-05.local
#   SKIP_BUILD=1 ./deploy_agent.sh pi@tv-02.local   # reuse the last build
#   USE_DOCKER=1 ./deploy_agent.sh pi@tv-03.local   # build in a Linux container
#
# Deploying to a fleet:
#   for n in 01 02 03; do ./deploy_agent.sh pi@tv-$n.local; done
# (the first run builds, the rest reuse the artifact)
#
# Build backend, in the order the script picks one:
#   USE_CARGO=1   host cargo, if it already has the target's linker
#   USE_DOCKER=1  cargo inside a Linux container of the target's architecture
#   otherwise     `cross` (cargo install cross), needs Docker
#
# USE_DOCKER is chosen automatically when the host can build the target
# natively — an arm64 host targeting aarch64. That is not a cross-compile at
# all: the container's own triple is the target. It is also the only route that
# works on an Apple Silicon Mac, where cross 0.2.5 assumes x86_64 Linux images
# and dies installing an x86_64 toolchain.
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

# The container image used by USE_DOCKER. Keep it on the oldest distribution
# the fleet runs: a binary runs against its own glibc or newer, never older,
# and Raspberry Pi OS is bookworm-based.
RUST_IMAGE="${RUST_IMAGE:-rust:1.97-slim-bookworm}"

case "$TARGET_ARCH" in
  aarch64-*) DOCKER_PLATFORM="linux/arm64" ;;
  armv7-*)   DOCKER_PLATFORM="linux/arm/v7" ;;
  arm-*)     DOCKER_PLATFORM="linux/arm/v6" ;;
  *)         DOCKER_PLATFORM="" ;;
esac

host_arch="$(uname -m)"
# True only when the container runs the target's architecture natively, so the
# build needs no emulation and no cross toolchain.
docker_builds_natively() {
  [[ "$DOCKER_PLATFORM" == "linux/arm64" ]] \
    && [[ "$host_arch" == "arm64" || "$host_arch" == "aarch64" ]] \
    && command -v docker >/dev/null
}

build_in_docker() {
  [[ -n "$DOCKER_PLATFORM" ]] || {
    echo "No container platform known for $TARGET_ARCH; use USE_CARGO=1 or cross." >&2
    exit 1
  }
  # $(id -u) and $TARGET_ARCH expand on the host, before the container sees them.
  # The chown hands artifacts back: cargo runs as root in there.
  # The registry cache is a named volume: without it every run re-downloads
  # the whole dependency tree, which dominates the time on a fleet loop.
  docker run --rm --platform "$DOCKER_PLATFORM" \
    -v "$REPO_DIR":/app -w /app \
    -v tv-agent-cargo-registry:/usr/local/cargo/registry \
    "$RUST_IMAGE" bash -c "
      set -euo pipefail
      apt-get update -qq
      apt-get install -y -qq --no-install-recommends build-essential >/dev/null
      cargo build --release -p pi-agent --target $TARGET_ARCH
      chown -R $(id -u):$(id -g) target/$TARGET_ARCH
    "
}

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  if [[ "${USE_CARGO:-}" == "1" ]]; then
    step "Building pi-agent for $TARGET_ARCH (host cargo)"
    (cd "$REPO_DIR" && cargo build --release -p pi-agent --target "$TARGET_ARCH")
  elif [[ "${USE_DOCKER:-}" == "1" ]] || docker_builds_natively; then
    command -v docker >/dev/null || {
      echo "USE_DOCKER=1 but docker was not found." >&2
      exit 1
    }
    if docker_builds_natively; then
      step "Building pi-agent for $TARGET_ARCH (native, in $RUST_IMAGE)"
    else
      step "Building pi-agent for $TARGET_ARCH (in $RUST_IMAGE, emulating $DOCKER_PLATFORM — slow)"
    fi
    build_in_docker
  else
    command -v cross >/dev/null || {
      echo "cross not found. Install it with 'cargo install cross', or set USE_DOCKER=1 (needs Docker) or USE_CARGO=1." >&2
      exit 1
    }
    step "Building pi-agent for $TARGET_ARCH (cross)"
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
