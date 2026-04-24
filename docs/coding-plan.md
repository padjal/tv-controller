# TV Controller — Rust Coding Plan

Full coding plan for a coding agent. Execute phases in order. Each task is self-contained and testable before moving to the next.

---

## Workspace layout

```
tv-controller/
├── Cargo.toml                  ← workspace root
├── Cross.toml                  ← cross-compilation targets
├── shared/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs              ← all shared types (serde + ts-rs)
├── server/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── router.rs           ← Axum route definitions
│       ├── db.rs               ← SQLx pool + migrations
│       ├── handlers/
│       │   ├── devices.rs
│       │   ├── videos.rs
│       │   ├── playback.rs
│       │   └── sse.rs          ← Server-Sent Events stream
│       ├── services/
│       │   ├── fan_out.rs      ← concurrent reqwest to Pi agents
│       │   ├── heartbeat.rs    ← background Tokio task
│       │   └── video_scan.rs   ← scan disk + ffprobe
│       └── state.rs            ← AppState shared across handlers
├── pi-agent/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── router.rs
│       ├── mpv.rs              ← Unix IPC socket wrapper
│       └── config.rs           ← env config, device ID persistence
└── dashboard/                  ← Vite + React (not a Cargo member)
    ├── package.json
    ├── vite.config.ts
    └── src/
        ├── App.tsx
        ├── types/              ← ts-rs writes generated files here
        ├── components/
        │   ├── TVGrid.tsx
        │   ├── VideoLibrary.tsx
        │   └── CommandBar.tsx
        ├── hooks/
        │   └── useSSE.ts       ← EventSource hook for live state
        └── api.ts
```

---

## Workspace `Cargo.toml`

```toml
[workspace]
members = ["shared", "server", "pi-agent"]
resolver = "2"

[workspace.dependencies]
shared      = { path = "shared" }
tokio       = { version = "1", features = ["full"] }
axum        = { version = "0.7", features = ["macros"] }
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
sqlx        = { version = "0.7", features = ["sqlite", "runtime-tokio", "macros"] }
reqwest     = { version = "0.12", features = ["json"] }
uuid        = { version = "1", features = ["v4", "serde"] }
ts-rs       = "10"
tracing     = "0.1"
tracing-subscriber = "0.3"
anyhow      = "1"
tokio-stream = "0.1"
```

---

## `Cross.toml` (cross-compilation)

```toml
[build.target.armv7-unknown-linux-gnueabihf]
# Pi 2/3/4/Zero 2W (ARMv7)
pre-build = ["apt-get update && apt-get install -y libasound2-dev"]

[build.target.arm-unknown-linux-gnueabihf]
# Pi 1 B+ / Zero W (ARMv6)
pre-build = ["apt-get update && apt-get install -y libasound2-dev"]
```

Build commands:
```bash
# For Pi Zero 2W / Pi 4
cross build --release --package pi-agent --target armv7-unknown-linux-gnueabihf

# For Pi 1 B+ / Zero W
cross build --release --package pi-agent --target arm-unknown-linux-gnueabihf

# Deploy
scp target/armv7-unknown-linux-gnueabihf/release/pi-agent pi@tv-01.local:/usr/local/bin/
```

---

## Phase 1 — `shared` crate

**This is built first. Everything else depends on it.**

### Task 1.1 — All wire types in `shared/src/lib.rs`

```rust
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

// ── Device ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub ip: String,
    pub state: DeviceState,
    pub current_video: Option<String>,
    pub last_seen: i64, // Unix timestamp
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub enum DeviceState {
    Idle,
    Playing,
    Paused,
    Offline,
}

// ── Video ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct Video {
    pub id: Uuid,
    pub filename: String,
    pub duration_secs: Option<u32>,
    pub size_bytes: u64,
}

// ── Commands (server → Pi agent) ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct PlayCommand {
    pub url: String,           // http://server:8000/videos/file.mp4
    pub video_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct PlaybackRequest {
    pub device_ids: Vec<Uuid>,
    pub video_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct StopRequest {
    pub device_ids: Vec<Uuid>,
}

// ── Agent status (Pi → server) ────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct AgentStatus {
    pub state: DeviceState,
    pub current_video_id: Option<Uuid>,
    pub position_secs: Option<f64>,
    pub duration_secs: Option<f64>,
}

// ── Registration (Pi → server on boot) ───────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct RegisterRequest {
    pub id: Uuid,
    pub name: String,
    pub ip: String,
}

// ── SSE envelope (server → dashboard) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub struct SseEvent {
    pub kind: SseKind,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub enum SseKind {
    DeviceUpdated,
    DeviceOffline,
    VideoLibraryChanged,
}
```

### Task 1.2 — `shared/Cargo.toml`

```toml
[package]
name    = "shared"
version = "0.1.0"
edition = "2021"

[dependencies]
serde      = { workspace = true }
serde_json = { workspace = true }
uuid       = { workspace = true }
ts-rs      = { workspace = true }
```

### Task 1.3 — Type generation

Add to `shared/Cargo.toml`:
```toml
[[test]]
name = "ts_export"
```

Create `shared/tests/ts_export.rs`:
```rust
#[test]
fn export_types() {
    use shared::*;
    Device::export_all().unwrap();
    Video::export_all().unwrap();
    PlayCommand::export_all().unwrap();
    PlaybackRequest::export_all().unwrap();
    AgentStatus::export_all().unwrap();
    SseEvent::export_all().unwrap();
}
```

Run with `cargo test -p shared` — this writes all `.ts` files into `dashboard/src/types/`.

---

## Phase 2 — Pi agent

Build and test this on a Pi (or in a Linux VM) before writing any server code.

### Task 2.1 — `pi-agent/src/config.rs`

- Read `SERVER_URL`, `DEVICE_NAME` from environment / `.env` file using `dotenvy`
- Read or generate `DEVICE_ID` (UUID):
    - Check `/etc/tv-agent/device.id`
    - If missing: generate `Uuid::new_v4()`, write to file, use it
- Expose a `Config` struct loaded once at startup

```rust
pub struct Config {
    pub server_url: String,   // e.g. "http://192.168.1.10:8000"
    pub device_name: String,  // e.g. "TV-01"
    pub device_id: Uuid,
    pub agent_port: u16,      // default 8080
}
```

### Task 2.2 — `pi-agent/src/mpv.rs`

mpv must be launched with `--input-ipc-server=/tmp/mpvsocket --idle=yes --no-terminal`.

Implement `MpvClient`:

```rust
pub struct MpvClient {
    socket_path: PathBuf,
}

impl MpvClient {
    pub async fn ensure_running(&self) -> anyhow::Result<()>
    // Checks if mpv process is running; spawns it if not
    // Uses tokio::process::Command

    pub async fn send_command(&self, cmd: serde_json::Value)
        -> anyhow::Result<serde_json::Value>
    // Opens UnixStream to socket_path
    // Writes JSON command + "\n"
    // Reads one line response
    // Returns parsed response

    pub async fn play(&self, url: &str) -> anyhow::Result<()>
    // send_command: {"command": ["loadfile", url, "replace"]}

    pub async fn stop(&self) -> anyhow::Result<()>
    // send_command: {"command": ["stop"]}

    pub async fn pause(&self) -> anyhow::Result<()>
    // send_command: {"command": ["set_property", "pause", true]}

    pub async fn resume(&self) -> anyhow::Result<()>
    // send_command: {"command": ["set_property", "pause", false]}

    pub async fn get_status(&self) -> anyhow::Result<AgentStatus>
    // Query properties: "pause", "time-pos", "duration", "idle-active"
    // Map to AgentStatus { state, position_secs, duration_secs, .. }
}
```

Key detail: mpv IPC is request/response but the socket is also used for event
notifications. Use a request ID field (`"request_id": N`) in every command and
match it in the response to avoid reading an event as a reply.

### Task 2.3 — `pi-agent/src/router.rs`

Axum router with shared `Arc<MpvClient>` state:

```
POST /play      body: PlayCommand       → mpv.play(url)
POST /stop      body: {}                → mpv.stop()
POST /pause     body: {}                → mpv.pause()
POST /resume    body: {}                → mpv.resume()
GET  /status    body: –                 → mpv.get_status() as JSON
GET  /health    body: –                 → { ok: true, hostname, ip }
```

All handlers return `Json<T>` and use `anyhow::Error` mapped to
`StatusCode::INTERNAL_SERVER_ERROR`.

### Task 2.4 — `pi-agent/src/main.rs`

```rust
#[tokio::main]
async fn main() {
    // 1. Load config
    // 2. Ensure mpv is running (MpvClient::ensure_running)
    // 3. Register with server: POST {server_url}/api/devices/register
    //    Body: RegisterRequest { id, name, ip }
    //    Retry with backoff if server unreachable at boot
    // 4. Start Axum on 0.0.0.0:{config.agent_port}
}
```

### Task 2.5 — Systemd service

`/etc/systemd/system/tv-agent.service`:
```ini
[Unit]
Description=TV Agent
After=network.target

[Service]
ExecStart=/usr/local/bin/pi-agent
Restart=always
RestartSec=5
EnvironmentFile=/etc/tv-agent/.env
User=pi

[Install]
WantedBy=multi-user.target
```

`/etc/tv-agent/.env`:
```
SERVER_URL=http://192.168.1.10:8000
DEVICE_NAME=TV-01
AGENT_PORT=8080
```

---

## Phase 3 — Server

### Task 3.1 — `server/src/db.rs`

SQLite migrations (use `sqlx::migrate!` macro pointing to `server/migrations/`).

`server/migrations/001_init.sql`:
```sql
CREATE TABLE IF NOT EXISTS devices (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    ip           TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'Idle',
    current_video TEXT,
    last_seen    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS videos (
    id             TEXT PRIMARY KEY,
    filename       TEXT NOT NULL UNIQUE,
    path           TEXT NOT NULL,
    size_bytes     INTEGER NOT NULL,
    duration_secs  INTEGER,
    added_at       INTEGER NOT NULL
);
```

Expose `Db` type alias: `pub type Db = sqlx::SqlitePool`.

### Task 3.2 — `server/src/state.rs`

```rust
pub struct AppState {
    pub db: Db,
    pub sse_tx: broadcast::Sender<SseEvent>,  // tokio broadcast channel
    pub server_base_url: String,              // e.g. "http://192.168.1.10:8000"
    pub videos_dir: PathBuf,
}
```

`sse_tx` is created at startup with `broadcast::channel(64)`. SSE handler
subscribes to it; other handlers publish into it.

### Task 3.3 — `server/src/services/video_scan.rs`

- Walk `videos_dir` for files with extensions: `.mp4 .mkv .mov .avi .webm`
- For each file, call `ffprobe` via `tokio::process::Command`:
  ```
  ffprobe -v quiet -print_format json -show_format <path>
  ```
  Parse `format.duration` as `f64` seconds.
- Upsert into `videos` table (skip if filename already exists with same size)
- Run once at startup, then watch directory with `notify` crate for changes
- On change: rescan, upsert, broadcast `SseEvent { kind: VideoLibraryChanged, .. }`

### Task 3.4 — `server/src/services/fan_out.rs`

```rust
pub async fn fan_out_play(
    devices: Vec<Device>,
    command: PlayCommand,
    client: &reqwest::Client,
) -> Vec<(Uuid, anyhow::Result<()>)>
```

- Use `futures::future::join_all` over a vec of `client.post(url).json(&command).send()` futures
- All requests fire concurrently, not sequentially
- Return per-device results so caller can update DB state individually
- Timeout: 5 seconds per request (`client` built with `.timeout(Duration::from_secs(5))`)

Same pattern for `fan_out_stop`, `fan_out_pause`, `fan_out_resume`.

### Task 3.5 — `server/src/services/heartbeat.rs`

Background Tokio task spawned in `main`:

```rust
pub async fn run_heartbeat(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let devices = db::list_devices(&state.db).await;
        for device in devices {
            let url = format!("http://{}:8080/status", device.ip);
            match client.get(&url).send().await {
                Ok(resp) => {
                    let status: AgentStatus = resp.json().await?;
                    // update device state + last_seen in DB
                    // broadcast SseEvent::DeviceUpdated
                }
                Err(_) => {
                    // if last_seen > 30s ago, mark Offline
                    // broadcast SseEvent::DeviceOffline
                }
            }
        }
    }
}
```

### Task 3.6 — `server/src/handlers/devices.rs`

```
POST /api/devices/register      body: RegisterRequest   → upsert device, broadcast DeviceUpdated
GET  /api/devices                                        → list all Device rows
GET  /api/devices/:id                                    → single device
DELETE /api/devices/:id                                  → remove device
```

### Task 3.7 — `server/src/handlers/videos.rs`

```
GET  /api/videos                 → list all Video rows
GET  /api/videos/:id             → single video metadata
GET  /videos/:filename           → ServeFile (Tower service)
```

The file endpoint uses Tower's `ServeDir` or a manual `tokio::fs::File` read
streamed via `axum::body::Body::from_stream`. Set `Content-Type` correctly and
support `Range` headers so mpv can seek before the full file downloads.

### Task 3.8 — `server/src/handlers/playback.rs`

```
POST /api/playback/play          body: PlaybackRequest
  1. Load video from DB → build URL: {server_base_url}/videos/{filename}
  2. Load target devices from DB
  3. fan_out::fan_out_play(devices, PlayCommand { url, video_id })
  4. For each success: update device state=Playing, current_video in DB
  5. Broadcast SseEvent::DeviceUpdated for each

POST /api/playback/stop          body: StopRequest
POST /api/playback/pause         body: StopRequest (same shape)
POST /api/playback/resume        body: StopRequest
POST /api/playback/play-all      body: { video_id }  → targets all non-Offline devices
```

### Task 3.9 — `server/src/handlers/sse.rs`

```rust
pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|msg| {
        let event = msg.unwrap();
        Ok(Event::default().json_data(event).unwrap())
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

Route: `GET /api/events`

### Task 3.10 — `server/src/main.rs`

```rust
#[tokio::main]
async fn main() {
    // 1. Init tracing (tracing_subscriber)
    // 2. Connect SQLite pool, run migrations
    // 3. Scan videos dir
    // 4. Build AppState with broadcast channel
    // 5. Spawn heartbeat task
    // 6. Build Axum router (all handlers + ServeDir for dashboard/dist)
    // 7. Bind 0.0.0.0:8000, serve
}
```

The router mounts the built dashboard at `/`:
```rust
Router::new()
    .nest("/api", api_router)
    .nest_service("/videos", ServeDir::new(&videos_dir))
    .fallback_service(ServeDir::new("dashboard/dist"))
```

---

## Phase 4 — Dashboard

### Task 4.1 — `dashboard/src/hooks/useSSE.ts`

```typescript
import { useEffect, useState } from "react";
import { SseEvent } from "../types/SseEvent";

export function useSSE(url: string) {
  const [lastEvent, setLastEvent] = useState<SseEvent | null>(null);
  useEffect(() => {
    const es = new EventSource(url);
    es.onmessage = (e) => setLastEvent(JSON.parse(e.data) as SseEvent);
    return () => es.close();
  }, [url]);
  return lastEvent;
}
```

### Task 4.2 — `dashboard/src/api.ts`

Typed wrappers using generated types from `shared`:
```typescript
import type { Device, Video, PlaybackRequest, StopRequest } from "./types";

const BASE = "";  // same origin

export const api = {
  getDevices: (): Promise<Device[]> =>
    fetch(`${BASE}/api/devices`).then(r => r.json()),

  getVideos: (): Promise<Video[]> =>
    fetch(`${BASE}/api/videos`).then(r => r.json()),

  play: (body: PlaybackRequest) =>
    fetch(`${BASE}/api/playback/play`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  stop: (body: StopRequest) =>
    fetch(`${BASE}/api/playback/stop`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),

  pause: (body: StopRequest) =>
    fetch(`${BASE}/api/playback/pause`, { method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body) }),

  resume: (body: StopRequest) =>
    fetch(`${BASE}/api/playback/resume`, { method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body) }),
};
```

### Task 4.3 — `TVGrid.tsx`

- Fetches devices on mount via `api.getDevices()`
- Subscribes to SSE via `useSSE("/api/events")`
- On `SseEvent { kind: "DeviceUpdated" }`, merges payload into local device list
- Renders a responsive grid of tiles (CSS grid, 4–5 columns)
- Each tile shows: name, state badge (colour-coded), current video name
- Click to toggle selection; selected tiles get a visible highlight ring
- Exports `selectedIds: Set<string>` via lifted state to parent

### Task 4.4 — `VideoLibrary.tsx`

- Fetches video list on mount
- Subscribes to SSE for `VideoLibraryChanged` to auto-refresh
- Renders scrollable list: filename, duration (formatted), size
- Click to select one video; highlights selected row
- Exports `selectedVideoId: string | null` to parent

### Task 4.5 — `CommandBar.tsx`

- Sticky bar at bottom of viewport
- Receives `selectedIds`, `selectedVideoId` as props
- **Play**: enabled when `selectedIds.size > 0 && selectedVideoId !== null`
- **Stop / Pause / Resume**: enabled when `selectedIds.size > 0`
- Shows loading spinner on in-flight request, disables all buttons during request
- Displays success/error toast after each command

### Task 4.6 — Build integration

`dashboard/vite.config.ts` — proxy API calls to Rust server during development:
```typescript
export default defineConfig({
  server: {
    proxy: {
      "/api": "http://localhost:8000",
      "/videos": "http://localhost:8000",
    },
  },
  build: {
    outDir: "../server/dashboard/dist",
  },
});
```

In production: `npm run build` → output lands in `server/dashboard/dist` →
served by Axum's `ServeDir` fallback.

---

## Phase 5 — Deployment

### Task 5.1 — SD card setup script `scripts/setup_pi.sh`

Run once to produce the golden image:
```bash
#!/usr/bin/env bash
# Run on a fresh Pi OS Lite install
apt-get update
apt-get install -y mpv ffprobe
mkdir -p /etc/tv-agent
useradd -r -s /bin/false tv-agent 2>/dev/null || true
```

After running: configure `.env`, enable systemd service, then `dd` the card as
the golden image for cloning.

### Task 5.2 — `scripts/deploy_agent.sh`

```bash
#!/usr/bin/env bash
TARGET=$1  # e.g. pi@tv-01.local
BINARY=target/armv7-unknown-linux-gnueabihf/release/pi-agent
scp $BINARY $TARGET:/usr/local/bin/pi-agent
ssh $TARGET "sudo systemctl restart tv-agent"
```

### Task 5.3 — `docker-compose.yml` (server)

```yaml
services:
  server:
    build: .
    ports:
      - "8000:8000"
    volumes:
      - ./videos:/app/videos
      - ./tv-controller.db:/app/tv-controller.db
    environment:
      - SERVER_BASE_URL=http://192.168.1.10:8000
      - DATABASE_URL=sqlite:///app/tv-controller.db
      - VIDEOS_DIR=/app/videos
    restart: unless-stopped
```

---

## Build order for the coding agent

1. `shared` crate — write all types, run `cargo test -p shared` to generate TS files
2. `pi-agent` — implement and test `mpv.rs` first with a manual `cargo run` on a Pi (or Linux VM with mpv installed); then add the HTTP router
3. `server` — implement handlers in this order: devices → videos → playback → SSE → heartbeat
4. `dashboard` — implement `useSSE` hook and `api.ts` first, then components bottom-up (VideoLibrary → TVGrid → CommandBar)
5. Integration test: one Pi, one server, one browser

---

## Key crate versions (pinned)

```toml
axum              = "0.7"
tokio             = "1"          # features = ["full"]
sqlx              = "0.7"        # features = ["sqlite", "runtime-tokio", "macros"]
reqwest           = "0.12"       # features = ["json"]
serde             = "1"          # features = ["derive"]
serde_json        = "1"
uuid              = "1"          # features = ["v4", "serde"]
ts-rs             = "10"
tracing           = "0.1"
tracing-subscriber = "0.3"
anyhow            = "1"
tokio-stream      = "0.1"
futures           = "0.3"
dotenvy           = "0.15"
notify            = "6"          # server: watch videos dir
tower-http        = "0.5"        # ServeDir for static files
```