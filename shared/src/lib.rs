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
    pub last_seen: i64,
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
    pub url: String,
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
    // commented out because it doesn't implement TS
    // pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../dashboard/src/types/")]
pub enum SseKind {
    DeviceUpdated,
    DeviceOffline,
    VideoLibraryChanged,
}
