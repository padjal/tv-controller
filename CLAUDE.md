# TV Controller

## Architecture
Cargo workspace: `shared`, `server`, `pi-agent`. React dashboard in `dashboard/`.
See `docs/coding-plan.md` for full task breakdown.

## Current state
- Phase 1 complete: `shared` crate with all wire types, TS export test passing, 10 `.ts` files in `dashboard/src/types/`
- Phase 2 complete: `pi-agent` — config, mpv IPC client, axum router, main with registration retry, systemd deploy files. Verified on Pi 5: health, status, play, pause, resume, stop all working over HTTP.
- Phase 3 complete: server — db, AppState, video_scan, fan_out, heartbeat, device/video/playback/SSE handlers, router and main. 95 tests. Verified live: all three `scripts/test/server/*.sh` suites pass against a running server, SSE carries events from all four sources, SIGTERM shuts down cleanly. `main` serves `/api` on `PORT` (default 8000) and runs the scanner + heartbeat; static file serving lands in 3.10.
- Phase 4 complete: dashboard — `useSSE`, `api.ts`, TVGrid, VideoLibrary, CommandBar, build integration. 69 tests. Built output is served by the Rust server; playback drives real agents end to end.
- Phase 5 (deployment) not started — `scripts/setup_pi.sh`, `scripts/deploy_agent.sh`, `docker-compose.yml`.

## Conventions
- All errors use `anyhow::Result` — no unwrap() outside tests
- All handlers return `Json<T>` — no plain string responses
- Database access only via functions in `server/src/db.rs` — no inline queries in handlers
- Run `cargo clippy -- -D warnings` before considering any task done
- Run `cargo test -p shared` after any type change to regenerate TS files
- Dashboard: `npm run typecheck`, `npm test`, `npm run build` from `dashboard/`. `build` typechecks first and writes to `server/dashboard/dist/`

## Known issues / decisions
- mpv IPC request_id matching: see comment in pi-agent/src/mpv.rs line 42
- mpv must be launched with a valid display or `--vo=null`; when run via SSH without DISPLAY/WAYLAND_DISPLAY set, video output fails silently and mpv stays idle — production systemd service must set the correct display environment
- Video file serving is `tower_http::services::ServeDir` mounted at `/videos` in `server/src/router.rs` — Range, Content-Type, HEAD, conditional requests and `..` rejection all come from it. Range behaviour is covered by tests (206, open-ended, suffix, 416) and by `scripts/test/server/videos.sh`; still not exercised from a real browser
- `ServeDir` serves anything in `videos_dir`, not just video extensions — keep non-video files out of that directory
- ts-rs 10 resolves `export_to` relative to the source file, not the crate root — use `../../dashboard/src/types/` (not `../`) from `shared/src/lib.rs`
- `serde_json::Value` does not implement `TS`; annotate fields with `#[ts(type = "unknown")]`
- reqwest uses `rustls-tls` with `default-features = false` — no OpenSSL dependency
- Server requires `SERVER_BASE_URL` and refuses to start without it — it is baked into the video URLs agents fetch, so a localhost default would only fail later, on the Pi. See `server/.env.example`
- `ffprobe` is optional: if it is missing, videos are still indexed and playable, just with `duration_secs = NULL`. The warning is logged once, not per file
- ffprobe reports `format.duration` as a JSON *string* ("30.024000"), and omits it entirely for some containers — see `parse_duration_secs` in `server/src/services/video_scan.rs`
- Video scanning is non-recursive and prunes rows whose file is gone; `videos_dir` must stay flat because `filename` is UNIQUE and the serve route is `/videos/:filename`
- `db.rs`, `state.rs`, `fan_out.rs` and `heartbeat.rs` carry a module-level `#![allow(dead_code)]` while Phase 3 is incomplete — remove them once the handlers land
- Playback status codes are meaningful because `api.ts` checks `res.ok`: 200 if at least one device accepted, 502 if every target failed, 409 for play-all with nothing online, 400 for empty `device_ids`, 404 for an unknown video. Per-device detail is in the `{succeeded, failed}` body
- A device that refuses a playback command gets no database write — its real state is unknown, and the heartbeat is what decides it is offline. Only successes are recorded and broadcast
- pause/resume keep `current_video`; stop clears it; play sets it
- Playback commands refresh `last_seen` on success, since a reply proves the agent is alive
- SSE: a subscriber that falls more than 64 events behind gets a named `lagged` frame, not a panic — the plan's sketch unwrapped the `Lagged` error, which would kill the connection. The current dashboard hook only reads unnamed messages so it ignores that frame; the warning is also logged. After a lag the receiver resumes at the oldest still-buffered event
- The dashboard fetches state then subscribes, so events published in between are missed. There is no snapshot-on-connect (that would need a new `SseKind`); a lagged or racing client stays stale until the next real change
- `/api` has its own 404 fallback. Without it an unknown API path would reach the SPA fallback and answer a `fetch()` with index.html and a 200
- The dashboard is served from `DASHBOARD_DIR` (default `dashboard/dist`, resolved against the working directory) with an index.html fallback, so client-side deep links and refreshes work
- The server handles SIGTERM and Ctrl-C, so `docker stop` and `systemctl restart` exit cleanly rather than being killed
- ts-rs maps `i64`/`u64` to TS `bigint`, which `JSON.parse` never produces — the wire carries plain JSON numbers. `Device::last_seen` and `Video::size_bytes` are pinned with `#[ts(type = "number")]`. Do the same for any new 64-bit field
- `useSSE` is callback-based, not last-event-state: a burst of `DeviceUpdated` events (playing on five TVs) coalesces into one React render, so a `lastEvent` hook would drop all but the last and leave tiles stale
- `api.ts` returns the body on a 502 instead of throwing — that status means every device refused, and the `{succeeded, failed}` detail is what a failure toast needs. Other error statuses throw `ApiError` carrying the server's message
- `PlaybackResult` in `api.ts` is hand-written to mirror `PlaybackResponse` in `server/src/handlers/playback.rs`, which is server-local rather than a `shared` type — keep them in step
- `dashboard/src/types/index.ts` is a hand-maintained barrel over the ts-rs output; add a line when adding a shared type
- Selection state is lifted to `App`; `TVGrid` takes `selectedIds` + `onToggle` so `CommandBar` can act on it. Each component owns the data it renders and its own SSE subscription — that is one EventSource per component, which is fine for two or three but worth consolidating if more are added
- Tiles are `<button aria-pressed>` rather than clickable divs, so selection works from the keyboard
- Dashboard styling is plain CSS: tokens and shell in `src/index.css`, one CSS file per component. No CSS framework
- The dashboard UI has never been opened in a real browser — tests are jsdom, so behaviour is covered but visual layout is not
- `VideoLibrary` uses native radio inputs (visually hidden, row styled via `:focus-within`) so single-select, arrow-key navigation and screen-reader announcement come for free
- `VideoLibrary` clears the selection when the selected file is pruned from disk, but only once a fetch has succeeded — an empty list while loading or after an error is not evidence the file is gone, and clearing on that wipes the selection on every remount
- `CommandBar` disables every button while a command is in flight, so two commands cannot race to set the same device's state
- `CommandBar` has a "Play on all" button that is not in the plan's button list; without it `/api/playback/play-all` would be unreachable from the UI. It needs only a video, since the server picks the targets
- Command results are reported per device: "Playing on 4 of 5" plus the server's error strings, which already name the failing TV. This is why `api.ts` returns the body on a 502 instead of throwing
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
- `scripts/test/server/videos.sh [SERVER_HOST]` — list → metadata → file download → Range → 416 → 404. Needs a video in `VIDEOS_DIR`; skips file tests if the library is empty
- `scripts/test/server/playback.sh [SERVER_HOST]` — play → pause → resume → stop → play-all → 400/404/502. Needs a registered device that is actually reachable (real pi-agent, or any stub answering POST /play,/stop,/pause,/resume on 8080) and one indexed video
- `scripts/test/server/events.sh [SERVER_HOST]` — tail the SSE stream, one line per event. Run it in one terminal and drive the server from another

`PI_HOST` defaults to `192.168.1.11`, `VIDEO_PATH` defaults to `/tmp/test.mp4`, `SERVER_HOST` defaults to `127.0.0.1` (port 8000). Override via env var or positional arg.

When adding new endpoints in any phase, add a corresponding script here.

## Do not
- Add unwrap() or expect() in non-test code
- Change shared types without regenerating TS files
- Skip clippy
