use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

// ── Device ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub ip: String,
    pub state: DeviceState,
    pub current_video: Option<String>,
    /// Display rotation in degrees clockwise: 0, 90, 180 or 270.
    pub rotation: u16,
    // Unix timestamp. Serialized as a JSON number, so the generated TS must
    // say `number` — ts-rs maps i64 to `bigint` by default, which JSON.parse
    // never produces.
    #[ts(type = "number")]
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub enum DeviceState {
    Idle,
    Playing,
    Paused,
    Offline,
}

// ── Media kind ────────────────────────────────────────────────────────────────

/// Extensions the library indexes, lower-case and without the leading dot.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "avi", "webm"];
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// What sort of file a library entry is.
///
/// Derived from the filename rather than stored in a column: `filename` is
/// UNIQUE and never changes for a given row, so a stored kind could only ever
/// drift out of step with the name it was derived from.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub enum MediaKind {
    // Default so that `PlayCommand::kind` can be `#[serde(default)]` — an agent
    // that has not been redeployed yet sends no kind, and everything that
    // existed before this field was added was a video.
    #[default]
    Video,
    Image,
}

impl MediaKind {
    /// `None` for anything the library does not index.
    ///
    /// Extension handling goes through `Path` rather than a plain rsplit so the
    /// edge cases stay exactly as they were: `.mp4` is a dotfile with no
    /// extension, and `clip.mp4.part` (a half-finished copy) has extension
    /// `part`. Neither is indexed.
    ///
    /// Note that `webm` (video) and `webp` (image) differ by one character.
    pub fn from_filename(name: &str) -> Option<Self> {
        let ext = std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();

        if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            Some(Self::Video)
        } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            Some(Self::Image)
        } else {
            None
        }
    }

    pub fn is_image(self) -> bool {
        matches!(self, Self::Image)
    }
}

// ── Video ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct Video {
    pub id: Uuid,
    pub filename: String,
    // Always None for an image: ffprobe reports a bogus one-frame duration
    // ("0.040000") for a JPEG, which would render as 0:00 in the library.
    pub duration_secs: Option<u32>,
    pub kind: MediaKind,
    // Same as Device::last_seen: a JSON number on the wire, not a bigint.
    #[ts(type = "number")]
    pub size_bytes: u64,
}

// ── Commands (server → Pi agent) ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct PlayCommand {
    pub url: String,
    pub video_id: Uuid,
    // `serde(default)` throughout: the agent deserializes this struct with no
    // tolerance for missing fields, so a required addition here would make an
    // agent that has not been redeployed reject playback outright.
    #[serde(default)]
    pub kind: MediaKind,
    /// Unix time in milliseconds at which every targeted TV should begin, and
    /// the epoch the looping timeline is measured from.
    ///
    /// `None` means start immediately — used when only one device is targeted,
    /// where there is nothing to synchronise against and a lead time would
    /// just be a delay.
    ///
    /// A JSON number, not a bigint: see `Device::last_seen`.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub start_at_unix_ms: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct PlaybackRequest {
    pub device_ids: Vec<Uuid>,
    pub video_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct StopRequest {
    pub device_ids: Vec<Uuid>,
}

/// `POST /rotation` on the agent — set mpv's display rotation.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct RotationCommand {
    pub degrees: u16,
}

/// `PUT /api/devices/:id/rotation` on the server.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct RotationRequest {
    pub rotation: u16,
}

/// The rotations a TV can be mounted at. Anything else is rejected rather than
/// passed to mpv, which would silently accept e.g. 45 and shear the picture.
pub const ALLOWED_ROTATIONS: [u16; 4] = [0, 90, 180, 270];

/// `POST /idle-background` on the agent — what to show when nothing is playing.
/// `None` clears it back to a black screen.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct IdleBackgroundCommand {
    pub url: Option<String>,
}

/// Fleet-wide settings, `GET`/`PUT /api/settings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct Settings {
    /// Filename of a library image shown on idle TVs, or `None` for black.
    pub idle_background: Option<String>,
    /// The same file as an absolute URL, filled in by the server on read.
    ///
    /// Agents need a URL and only the server can build one — it owns the base
    /// address and the percent-encoding that a library full of spaces and `#`
    /// requires. Ignored on write; `serde(default)` so a client can PUT the
    /// filename alone.
    #[serde(default)]
    pub idle_background_url: Option<String>,
}

// ── Agent status (Pi → server) ────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct AgentStatus {
    pub state: DeviceState,
    pub current_video_id: Option<Uuid>,
    pub position_secs: Option<f64>,
    pub duration_secs: Option<f64>,
}

// ── Registration (Pi → server on boot) ───────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct RegisterRequest {
    pub id: Uuid,
    pub name: String,
    pub ip: String,
}

// ── SSE envelope (server → dashboard) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub struct SseEvent {
    pub kind: SseKind,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../dashboard/src/types/")]
pub enum SseKind {
    DeviceUpdated,
    DeviceOffline,
    VideoLibraryChanged,
    SettingsChanged,
}
