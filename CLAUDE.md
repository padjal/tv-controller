# TV Controller

## Architecture
Cargo workspace: `shared`, `server`, `pi-agent`. React dashboard in `dashboard/`.
See `docs/coding-plan.md` for full task breakdown.

## Current state
- Phase 1 complete: `shared` crate with all wire types, TS export test passing, 10 `.ts` files in `dashboard/src/types/`
- Phase 2 complete: `pi-agent` — config, mpv IPC client, axum router, main with registration retry, systemd deploy files. Verified on Pi 5: health, status, play, pause, resume, stop all working over HTTP.
- Phase 3 (server), Phase 4 (dashboard), Phase 5 (deployment) not started

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

## Test scripts
Bash scripts live in `scripts/test/<component>/`. Run from Git Bash on Windows or directly on the relevant host. All require `curl` and `jq`.
- `scripts/test/pi-agent/smoke.sh [PI_HOST] [VIDEO_PATH]` — full sequence: health → play → pause → resume → stop
- `scripts/test/pi-agent/play.sh [PI_HOST] [VIDEO_PATH]` — trigger playback
- `scripts/test/pi-agent/status.sh [PI_HOST]` — show current state

`PI_HOST` defaults to `192.168.1.11`, `VIDEO_PATH` defaults to `/tmp/test.mp4`. Override via env var or positional arg.

When adding new endpoints in any phase, add a corresponding script here.

## Do not
- Add unwrap() or expect() in non-test code
- Change shared types without regenerating TS files
- Skip clippy
