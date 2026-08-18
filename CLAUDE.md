# TV Controller

## Architecture
Cargo workspace: `shared`, `server`, `pi-agent`. React dashboard in `dashboard/`.
See `docs/coding-plan.md` for full task breakdown.

## Current state
- Phase 1 complete: `shared` crate with all wire types, TS export test passing, 10 `.ts` files in `dashboard/src/types/`
- Phase 2 complete: `pi-agent` — config, mpv IPC client, axum router, main with registration retry, systemd deploy files. Verified on Pi 5: health, status, play, pause, resume, stop all working over HTTP.
- Phase 3 (server) in progress: 3.1 db, 3.2 AppState, 3.3 video_scan, 3.4 fan_out, 3.5 heartbeat, 3.6 device handlers done. Next: 3.7 video handlers. `main` serves `/api` on `PORT` (default 8000) and runs the scanner + heartbeat; static file serving lands in 3.10.
- Phase 4 (dashboard), Phase 5 (deployment) not started

## Conventions
- All errors use `anyhow::Result` — no unwrap() outside tests
- All handlers return `Json<T>` — no plain string responses
- Database access only via functions in `server/src/db.rs` — no inline queries in handlers
- Run `cargo clippy -- -D warnings` before considering any task done
- Run `cargo test -p shared` after any type change to regenerate TS files

## Known issues / decisions
- mpv IPC request_id matching: see comment in pi-agent/src/mpv.rs line 42
- mpv must be launched with a valid display or `--vo=null`; when run via SSH without DISPLAY/WAYLAND_DISPLAY set, video output fails silently and mpv stays idle — production systemd service must set the correct display environment
- Video file serving uses Range headers — tested with mpv, not browser
- ts-rs 10 resolves `export_to` relative to the source file, not the crate root — use `../../dashboard/src/types/` (not `../`) from `shared/src/lib.rs`
- `serde_json::Value` does not implement `TS`; annotate fields with `#[ts(type = "unknown")]`
- reqwest uses `rustls-tls` with `default-features = false` — no OpenSSL dependency
- Server requires `SERVER_BASE_URL` and refuses to start without it — it is baked into the video URLs agents fetch, so a localhost default would only fail later, on the Pi. See `server/.env.example`
- `ffprobe` is optional: if it is missing, videos are still indexed and playable, just with `duration_secs = NULL`. The warning is logged once, not per file
- ffprobe reports `format.duration` as a JSON *string* ("30.024000"), and omits it entirely for some containers — see `parse_duration_secs` in `server/src/services/video_scan.rs`
- Video scanning is non-recursive and prunes rows whose file is gone; `videos_dir` must stay flat because `filename` is UNIQUE and the serve route is `/videos/:filename`
- `db.rs`, `state.rs`, `fan_out.rs` and `heartbeat.rs` carry a module-level `#![allow(dead_code)]` while Phase 3 is incomplete — remove them once the handlers land
- Server assumes every agent is on port 8080 (`AGENT_PORT` in `server/src/services/mod.rs`); `devices` has no port column, so an agent moved off the default is unreachable
- Heartbeat broadcasts only when a device's state or current video actually changes, not every 10s tick — `last_seen` is still persisted each round
- A device is marked Offline only after 30s of silence, and announced once; `last_seen` is left at its old value so it records when the device was last actually seen
- The stale-`Playing`-after-reboot case (registration preserves playback state) is corrected by the heartbeat within one poll — covered by a test in `heartbeat.rs`
- Agents report `current_video_id` (a Uuid) but `devices.current_video` stores the filename the dashboard displays, so the heartbeat resolves ids via one `list_videos` query per round
- Handler errors go through `server/src/error.rs` and always respond as JSON `{"error": "..."}`; `anyhow::Error` becomes a 500 with the full cause chain logged, not sent. `ApiError::not_found` / `bad_request` for 404/400
- `DELETE /api/devices/:id` publishes no SSE event — `SseKind` has no removed variant, so other open dashboards keep the tile until they refresh. Adding one means a `shared` change plus regenerating TS

## Test scripts
Bash scripts live in `scripts/test/<component>/`. Run from Git Bash on Windows or directly on the relevant host. All require `curl` and `jq`.
- `scripts/test/pi-agent/smoke.sh [PI_HOST] [VIDEO_PATH]` — full sequence: health → play → pause → resume → stop
- `scripts/test/pi-agent/play.sh [PI_HOST] [VIDEO_PATH]` — trigger playback
- `scripts/test/pi-agent/status.sh [PI_HOST]` — show current state

- `scripts/test/server/devices.sh [SERVER_HOST]` — register (twice, for idempotency) → list → get → 404 → 400 → delete

`PI_HOST` defaults to `192.168.1.11`, `VIDEO_PATH` defaults to `/tmp/test.mp4`, `SERVER_HOST` defaults to `127.0.0.1` (port 8000). Override via env var or positional arg.

When adding new endpoints in any phase, add a corresponding script here.

## Do not
- Add unwrap() or expect() in non-test code
- Change shared types without regenerating TS files
- Skip clippy
