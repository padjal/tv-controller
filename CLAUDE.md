# TV Controller

## Architecture
Cargo workspace: `shared`, `server`, `pi-agent`. React dashboard in `dashboard/`.
See `docs/coding-plan.md` for full task breakdown.

## Current state
- Phase 1 complete: `shared` crate with all wire types, TS export test passing, 10 `.ts` files in `dashboard/src/types/`
- Phase 2 complete: `pi-agent` — config, mpv IPC client, axum router, main with registration retry, systemd deploy files
- Phase 3 (server), Phase 4 (dashboard), Phase 5 (deployment) not started

## Conventions
- All errors use `anyhow::Result` — no unwrap() outside tests
- All handlers return `Json<T>` — no plain string responses
- Database access only via functions in `server/src/db.rs` — no inline queries in handlers
- Run `cargo clippy -- -D warnings` before considering any task done
- Run `cargo test -p shared` after any type change to regenerate TS files

## Known issues / decisions
- mpv IPC request_id matching: see comment in pi-agent/src/mpv.rs line 42
- Video file serving uses Range headers — tested with mpv, not browser
- ts-rs 10 resolves `export_to` relative to the source file, not the crate root — use `../../dashboard/src/types/` (not `../`) from `shared/src/lib.rs`
- `serde_json::Value` does not implement `TS`; annotate fields with `#[ts(type = "unknown")]`

## Do not
- Add unwrap() or expect() in non-test code
- Change shared types without regenerating TS files
- Skip clippy