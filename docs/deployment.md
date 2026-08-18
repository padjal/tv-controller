# Deployment guide

Two things get deployed: **the server** (one, usually in Docker) and **the
agent** (one per TV, on a Raspberry Pi). They find each other over the LAN — the
agent registers itself with the server on start, and the server polls it every
10 seconds thereafter.

- [Deploying the server](#deploying-the-server)
- [Setting up the first Pi](#setting-up-the-first-pi)
- [Cloning the fleet](#cloning-the-fleet)
- [Adding videos](#adding-videos)
- [Upgrading](#upgrading)
- [Troubleshooting](#troubleshooting)
- [A note on security](#a-note-on-security)

---

## Deploying the server

### With Docker (recommended)

```bash
cp .env.example .env
```

Edit `.env` and set `SERVER_BASE_URL` to the address the **Pis** will use:

```
SERVER_BASE_URL=http://192.168.1.10:8000
```

This is the one setting that matters. It gets baked into the video URLs the
agents are told to fetch, so `localhost` produces a server that starts happily
and then fails on every playback attempt. The server refuses to start without
it rather than guessing.

```bash
docker compose up -d --build
docker compose logs -f
```

The image builds the dashboard and the server from source, so no local Node or
Rust toolchain is needed.

**What the compose file mounts:**

| Path | Purpose |
| --- | --- |
| `./videos` → `/app/videos` (read-only) | Your video library |
| `tv-controller-data` (named volume) | The SQLite database |

The database lives in a named volume rather than a bind mount so it is owned by
the container's non-root user without any `chown` on the host. To back it up:

```bash
docker compose exec server cat /app/data/tv-controller.db > backup.db
```

> The original plan mounted `./tv-controller.db` as a file. That is fragile —
> Docker creates a *directory* by that name if the file does not exist yet, and
> the resulting error is confusing. A named volume avoids it.

### Without Docker

```bash
cp server/.env.example server/.env   # then edit SERVER_BASE_URL
cd dashboard && npm ci && npm run build && cd ..
cargo build --release -p server
./target/release/server
```

`server/.env.example` documents every variable. Relative paths (`VIDEOS_DIR`,
`DASHBOARD_DIR`, the SQLite file) resolve against the working directory, so run
it from the repo root or use absolute paths.

Install `ffmpeg` if you want video durations in the library — without `ffprobe`
the files still index and play, they just show no duration.

The server handles `SIGTERM`, so it is safe under systemd or `docker stop`.

---

## Setting up the first Pi

Start from a fresh **Raspberry Pi OS** install (Lite is enough — the agent
drives the display directly, no desktop required). Give the Pi a stable
hostname; `tv-01`, `tv-02` and so on make the fleet easier to reason about.

**1. Provision it**

With the repo available on the Pi (clone it, or copy `scripts/` and
`pi-agent/deploy/` across):

```bash
sudo ./scripts/setup_pi.sh
```

This installs `mpv`, adds the run user to the `video`, `render` and `audio`
groups, creates `/etc/tv-agent`, installs the systemd unit, writes an
`mpv.conf`, and disables console blanking. It does *not* install the agent
binary — that comes from your workstation, so the SD card can be imaged before
the binary is final.

Pass `RUN_USER=someone` if the agent should not run as `pi`.

**It detects the display session** and configures both files to match, because
getting this wrong is the failure that costs the most time — mpv accepts every
command and plays to nothing. It reports what it found:

```
== Detecting the display session
Wayland session, WAYLAND_DISPLAY=wayland-0
```

| Detected | `mpv.conf` | systemd unit |
| --- | --- | --- |
| `lite` (no session) | `vo=gpu`, `gpu-context=drm` | neither display line set |
| `wayland` | no `vo`/`gpu-context` | `WAYLAND_DISPLAY=<socket>` |
| `x11` | no `vo`/`gpu-context` | `DISPLAY=:<n>` |

Detection reads the live session: a socket in `/run/user/<uid>/wayland-*`, else
`/tmp/.X11-unix/X<n>`, else Lite. The Wayland socket is not always `wayland-0`,
so the real name is used.

Override it with `SESSION=lite|wayland|x11` — needed when you provision a card
for a machine other than the one you are on, or when the desktop session is not
running yet. If no session is found but the default target is
`graphical.target`, the script warns rather than silently choosing Lite.

An existing `mpv.conf` is never overwritten. If it sets `gpu-context=drm` while
a desktop session is running, the script warns — that combination cannot work,
since the compositor already holds DRM.

**2. Configure it**

```bash
sudo nano /etc/tv-agent/.env
```

```
SERVER_URL=http://192.168.1.10:8000
DEVICE_NAME=TV-01
AGENT_PORT=8080
```

`DEVICE_NAME` is what you will see on the dashboard tile. `AGENT_PORT` should
stay at 8080: the server assumes every agent is on that port, because the
device table has no port column.

The agent generates a UUID on first start and saves it to
`/etc/tv-agent/device.id`. That file is what makes a Pi the *same* device
across reboots and re-registrations — see [Cloning the fleet](#cloning-the-fleet)
for why it matters when imaging cards.

**3. Deploy the binary**

From your workstation:

```bash
cargo install cross                       # once; needs Docker
./scripts/deploy_agent.sh pi@tv-01.local
```

The script cross-compiles for `aarch64-unknown-linux-gnu` (Pi 3/4/5 on a 64-bit
OS), copies the binary, restarts the service, and checks `/health`. For a 32-bit
image, set `TARGET_ARCH=armv7-unknown-linux-gnueabihf`; `Cross.toml` has the
targets already configured.

**On an Apple Silicon Mac, `cross` does not work.** Version 0.2.5 assumes
x86_64 Linux images and dies trying to install an x86_64 toolchain:

```
error: toolchain 'stable-x86_64-unknown-linux-gnu' may not be able to run on this system
```

You do not need it there. An arm64 host runs an arm64 Linux container, whose
own triple *is* `aarch64-unknown-linux-gnu`, so the build is native rather than
cross — faster, and with no emulation:

```bash
docker run --rm -v "$PWD":/app -w /app rust:1.97-slim-bookworm bash -c '
  apt-get update -qq && apt-get install -y -qq --no-install-recommends build-essential
  cargo build --release -p pi-agent --target aarch64-unknown-linux-gnu
  chown -R $(id -u):$(id -g) target/aarch64-unknown-linux-gnu'   # id from the host
./scripts/deploy_agent.sh pi@tv-01.local     # SKIP_BUILD=1 also works
```

The binary lands where `deploy_agent.sh` looks for it, so the deploy step is
unchanged. Build on `bookworm` (glibc 2.36) rather than something newer: a
binary runs against its own glibc or later, never an older one, and Raspberry
Pi OS is bookworm-based.

It installs over a non-interactive SSH session, so the account needs
passwordless sudo — the default on Raspberry Pi OS. Set up your SSH key first
(`ssh-copy-id pi@tv-01.local`) or every deployment will ask for a password
twice.

**4. Enable it**

```bash
sudo systemctl enable --now tv-agent
sudo reboot                # group membership needs a fresh login
```

After the reboot, `curl localhost:8080/health` on the Pi, and the tile should
appear on the dashboard.

### If the video does not appear on screen

This is the most common failure, and it is silent: mpv accepts every command
and reports success while rendering to nothing.

The agent spawns `mpv` with no video-output flags, so output is configured
outside the code — in `~/.config/mpv/mpv.conf` (written by `setup_pi.sh`) and
in the systemd unit's environment.

- **Pi OS Lite**: the generated `mpv.conf` uses `vo=gpu` with
  `gpu-context=drm`. Nothing else may hold the display — if you also run a
  desktop session or another mpv, DRM is already taken.
- **Pi OS Desktop**: `setup_pi.sh` configures this for you when a session is
  running. If it ran before the desktop started, re-run it (or pass
  `SESSION=wayland` / `SESSION=x11`), then
  `sudo systemctl daemon-reload && sudo systemctl restart tv-agent`. To check
  what the service actually got:
  `systemctl show tv-agent -p Environment --value`.

To confirm it is a display problem rather than a delivery problem, SSH in and
run `mpv --vo=null <url>` by hand: if that plays, the file and the network are
fine and the output configuration is at fault.

---

## Cloning the fleet

Once the first Pi works end to end, image it rather than repeating the setup.

```bash
sudo systemctl stop tv-agent
sudo rm /etc/tv-agent/device.id     # ← the important step
sudo shutdown -h now
```

**Delete `device.id` before imaging.** It is the device's identity: every Pi
cloned from an image that still contains it will register as the *same* device,
and the tiles will fight over one row in the database. With the file absent,
each Pi generates a fresh UUID on first boot.

Then `dd` the card on another machine:

```bash
sudo dd if=/dev/sdX of=tv-golden.img bs=4M status=progress
```

For each new TV: write the image, boot it, then

```bash
sudo hostnamectl set-hostname tv-02
sudo nano /etc/tv-agent/.env        # DEVICE_NAME=TV-02
sudo reboot
```

Nothing else is needed — the binary is already on the image, and the agent
registers itself.

To push a new agent build across the fleet:

```bash
for n in 01 02 03 04; do ./scripts/deploy_agent.sh pi@tv-$n.local; done
```

The first run builds; the rest reuse the artifact.

---

## Adding videos

Copy files into the `videos/` directory next to `docker-compose.yml` (or
whatever `VIDEOS_DIR` points at). The server watches the directory and picks
up changes within a second or two — no restart, and the dashboard's library
refreshes itself over SSE.

Two constraints, both enforced by how files are served:

- **Keep the directory flat.** Scanning is non-recursive, filenames must be
  unique, and the serve route is `/videos/<filename>`. Subdirectories are
  ignored.
- **Keep non-video files out.** Everything in that directory is served over
  HTTP, not just the files that got indexed.

Deleting a file removes it from the library. If it was the video selected in
the dashboard, the selection clears on the next refresh.

---

## Upgrading

**Server:**

```bash
git pull
docker compose up -d --build
```

The database is in a volume and survives. Migrations run automatically at
startup.

**Agents:** re-run `deploy_agent.sh` per host, as above. The server tolerates an
agent disappearing and reappearing — the tile goes offline and comes back.

---

## Troubleshooting

**A TV never appears on the dashboard**

Check the agent is running and can see the server:

```bash
ssh pi@tv-01.local 'systemctl status tv-agent; journalctl -u tv-agent -n 50 --no-pager'
```

Registration retries with backoff, so "failed to reach server, retrying" in the
log means the agent is healthy and `SERVER_URL` is wrong or the server is down.

**A tile is stuck on "offline"**

The server polls each agent every 10 seconds and marks it offline after 30
seconds of silence. From the server host:

```bash
curl http://tv-01.local:8080/health
```

If that answers, the server cannot reach the Pi — check that the agent is on
port 8080, and that the IP the Pi registered with is still its IP. The agent
reports the address it finds by opening a socket outbound, which can be wrong
on a Pi with several interfaces.

**Playback reports success but nothing plays**

Almost always the display configuration — see
[If the video does not appear on screen](#if-the-video-does-not-appear-on-screen).

**Playback fails with an error naming the device**

The failure text comes from the agent and is shown per device in the
dashboard's result message. A common cause is the Pi being unable to fetch the
video: check `SERVER_BASE_URL` is the server's LAN address and not `localhost`,
then try the URL from the Pi with `curl -I`.

**The dashboard loads but shows nothing**

If the server logs `no dashboard build found` at startup, `DASHBOARD_DIR` is
wrong or the dashboard was never built. In Docker this cannot happen; outside
it, run `npm run build` in `dashboard/`.

**Checking the event stream**

```bash
./scripts/test/server/events.sh <server-host>
```

prints one line per SSE event. Run it in one terminal and drive the dashboard
from another — it is the fastest way to tell a server problem from a browser
problem.

---

## A note on security

There is **no authentication anywhere in this system**. Anyone who can reach
the server can control every TV and download every video; anyone who can reach
a Pi on port 8080 can control it directly, bypassing the server entirely.

That is a reasonable trade for an isolated LAN, which is what this is designed
for. It is not safe to expose to the internet or to an untrusted network. If
you need to reach it remotely, put it behind a VPN rather than forwarding a
port.
