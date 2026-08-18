# TV Controller

Play video on a wall of TVs from one browser tab.

Each TV has a Raspberry Pi behind it running a small agent that drives `mpv`.
A central server keeps track of which Pis are alive, serves the video files,
and fans commands out to them. The dashboard is a React app served by that same
server, so there is one address to remember and nothing to install on the
machine you control it from.

```
   browser  ──────────►  server (Rust/axum)  ──────────►  pi-agent  ──►  mpv  ──►  TV
             dashboard    ├── REST API                     one per TV
             + SSE        ├── video files over HTTP
                          └── SQLite (devices, video index)
```

## What it does

- **Select any set of TVs** and play, pause, resume or stop them together
- **Play on all** — one button, every TV that is online
- **A shared video library** — drop files in a directory; the server indexes
  them automatically and the agents stream them over HTTP
- **Live status** — tiles update over Server-Sent Events as devices come and
  go, so the dashboard is never stale by more than one heartbeat
- **Survives reboots** — an agent re-registers itself on start, and a Pi that
  drops off is marked offline after 30 seconds

## Quick start

You need Docker on the server machine, and one Raspberry Pi per TV.

**1. Start the server**

```bash
git clone <this repo> && cd tv-controller
cp .env.example .env
# Set SERVER_BASE_URL to this machine's LAN address — the Pis fetch video
# from it, so localhost will not do.
docker compose up -d --build
```

Drop a few `.mp4` files into `videos/` and open `http://<this-machine>:8000`.

**2. Set up a Pi**

On a fresh Raspberry Pi OS install, with the repo checked out:

```bash
sudo ./scripts/setup_pi.sh
sudo nano /etc/tv-agent/.env     # SERVER_URL and DEVICE_NAME
sudo systemctl enable --now tv-agent
```

**3. Deploy the agent binary from your workstation**

```bash
cargo install cross            # once
./scripts/deploy_agent.sh pi@tv-01.local
```

On an Apple Silicon Mac `cross` does not work; build the agent in an arm64
container instead — see [docs/deployment.md](docs/deployment.md#setting-up-the-first-pi).

The TV appears in the dashboard within a few seconds.

Full instructions, including cloning one SD card across a fleet:
**[docs/deployment.md](docs/deployment.md)**.
Day-to-day operation: **[docs/user-guide.md](docs/user-guide.md)**.

## Repository layout

| Path | What it is |
| --- | --- |
| `shared/` | Wire types shared by server and agent; generates the dashboard's TypeScript types via ts-rs |
| `server/` | The central server: REST API, SSE stream, video indexing, heartbeat |
| `pi-agent/` | The per-TV agent: HTTP control surface over an mpv IPC socket |
| `dashboard/` | React dashboard, built into `server/dashboard/dist` |
| `scripts/` | Provisioning, deployment, and endpoint test scripts |
| `docs/` | Deployment guide, user guide, and the original coding plan |

## Development

```bash
cargo test --workspace          # 109 tests
cargo clippy -- -D warnings

cd dashboard
npm ci
npm test                        # 69 tests
npm run dev                     # proxies /api and /videos to localhost:8000
```

Run the server locally with `cp server/.env.example server/.env`, then
`cargo run -p server`. The dashboard dev server proxies to it, so both reload
independently.

There are also endpoint scripts under `scripts/test/` that exercise a running
server or agent with `curl` — see the list in [CLAUDE.md](CLAUDE.md).

## Requirements

- **Server**: Docker, or a Rust toolchain and `ffmpeg` (for `ffprobe`; optional,
  it only supplies video durations)
- **Pi**: Raspberry Pi OS, `mpv`, network access to the server
- Everything runs on a LAN. There is no authentication — see the security note
  in [docs/deployment.md](docs/deployment.md#a-note-on-security).
