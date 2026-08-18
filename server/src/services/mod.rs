pub mod fan_out;
pub mod video_scan;

/// Port every Pi agent listens on.
///
/// The agent's own port is configurable via `AGENT_PORT`, but `devices` stores
/// no port column, so the server assumes the default. An agent moved off 8080
/// becomes unreachable — that would need a port column and a `RegisterRequest`
/// field to fix properly.
pub const AGENT_PORT: u16 = 8080;
