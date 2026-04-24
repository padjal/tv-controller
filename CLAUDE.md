# TV Controller

## Architecture
Cargo workspace: `shared`, `server`, `pi-agent`. React dashboard in `dashboard/`.
See `docs/coding-plan.md` for full task breakdown.

## Current state
Nothing is completed yet.

## Conventions
- All errors use `anyhow::Result` — no unwrap() outside tests
- All handlers return `Json<T>` — no plain string responses
- Database access only via functions in `server/src/db.rs` — no inline queries in handlers
- Run `cargo clippy -- -D warnings` before considering any task done
- Run `cargo test -p shared` after any type change to regenerate TS files

## Known issues / decisions
- mpv IPC request_id matching: see comment in pi-agent/src/mpv.rs line 42
- Video file serving uses Range headers — tested with mpv, not browser

## Do not
- Add unwrap() or expect() in non-test code
- Change shared types without regenerating TS files
- Skip clippy