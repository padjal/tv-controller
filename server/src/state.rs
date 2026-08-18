//! Shared application state, handed to every handler as `State<Arc<AppState>>`.
//!
//! Note: the SSE channel and URL helpers are consumed by later Phase 3 tasks
//! (playback handlers, video scan, heartbeat, SSE) and are not wired in yet,
//! hence the module-level `dead_code` allow. Remove it once the phase is
//! complete.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde::Serialize;
use shared::{SseEvent, SseKind};
use tokio::sync::broadcast;

use crate::db::Db;

/// Buffered SSE events per subscriber. A dashboard that falls this far behind
/// gets a `Lagged` error and resyncs rather than stalling every publisher.
const SSE_CHANNEL_CAPACITY: usize = 64;

/// Characters escaped when a filename is placed into a URL path segment.
/// This is the `url` crate's PATH set, plus `%` and `/` so a filename can never
/// escape its own segment.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'%')
    .add(b'/');

pub struct AppState {
    pub db: Db,
    pub sse_tx: broadcast::Sender<SseEvent>,
    /// Shared client for talking to agents. One pooled client for the whole
    /// process, rather than a fresh one per command.
    pub http: reqwest::Client,
    /// Absolute base the Pi agents use to reach this server, e.g.
    /// `http://192.168.1.10:8000`. Stored without a trailing slash.
    pub server_base_url: String,
    pub videos_dir: PathBuf,
}

impl AppState {
    pub fn new(
        db: Db,
        server_base_url: &str,
        videos_dir: PathBuf,
        http: reqwest::Client,
    ) -> Arc<Self> {
        let (sse_tx, _rx) = broadcast::channel(SSE_CHANNEL_CAPACITY);
        Arc::new(AppState {
            db,
            sse_tx,
            http,
            server_base_url: server_base_url.trim_end_matches('/').to_string(),
            videos_dir,
        })
    }

    /// Build state from the environment: `SERVER_BASE_URL` and `VIDEOS_DIR`.
    ///
    /// `SERVER_BASE_URL` has no useful default — it is the address the Pi agents
    /// fetch video from, so a wrong guess fails at playback time on a remote
    /// machine rather than here at startup.
    pub fn from_env(db: Db) -> Result<Arc<Self>> {
        let server_base_url = std::env::var("SERVER_BASE_URL")
            .context("SERVER_BASE_URL must be set (e.g. http://192.168.1.10:8000)")?;
        let videos_dir = std::env::var("VIDEOS_DIR").unwrap_or_else(|_| "videos".to_string());
        Ok(Self::new(
            db,
            &server_base_url,
            PathBuf::from(videos_dir),
            crate::services::fan_out::build_client()?,
        ))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SseEvent> {
        self.sse_tx.subscribe()
    }

    /// Publish an event to every connected dashboard.
    ///
    /// Broadcasting is best-effort by design: `send` fails when nobody is
    /// subscribed, which is the normal state with no dashboard open, and a
    /// payload that will not serialize must not abort the playback command that
    /// produced it. Both cases are logged and swallowed.
    pub fn broadcast(&self, kind: SseKind, payload: &impl Serialize) {
        let payload = match serde_json::to_value(payload) {
            Ok(value) => value,
            Err(err) => {
                tracing::error!(?kind, %err, "failed to serialize SSE payload; dropping event");
                return;
            }
        };

        if let Err(err) = self.sse_tx.send(SseEvent { kind, payload }) {
            tracing::debug!(%err, "no SSE subscribers; event dropped");
        }
    }

    /// The URL a Pi agent should fetch `filename` from.
    pub fn video_url(&self, filename: &str) -> String {
        let encoded = utf8_percent_encode(filename, PATH_SEGMENT);
        format!("{}/videos/{}", self.server_base_url, encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::Device;
    use sqlx::sqlite::SqlitePoolOptions;

    /// The pool is never queried in these tests, but building one still needs a
    /// Tokio context — sqlx spawns a connection reaper — so every caller must be
    /// an async test.
    fn state(base_url: &str) -> Arc<AppState> {
        let db = SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .unwrap();
        AppState::new(
            db,
            base_url,
            PathBuf::from("videos"),
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn video_url_joins_base_and_filename() {
        let s = state("http://192.168.1.10:8000");
        assert_eq!(
            s.video_url("clip.mp4"),
            "http://192.168.1.10:8000/videos/clip.mp4"
        );
    }

    #[tokio::test]
    async fn trailing_slash_on_base_url_does_not_double_up() {
        let s = state("http://192.168.1.10:8000/");
        assert_eq!(s.server_base_url, "http://192.168.1.10:8000");
        assert_eq!(
            s.video_url("clip.mp4"),
            "http://192.168.1.10:8000/videos/clip.mp4"
        );
    }

    #[tokio::test]
    async fn video_url_escapes_awkward_filenames() {
        let s = state("http://host:8000");
        assert_eq!(
            s.video_url("summer promo #2.mp4"),
            "http://host:8000/videos/summer%20promo%20%232.mp4"
        );
        // A filename must never break out of its own path segment.
        assert_eq!(
            s.video_url("../secrets.mp4"),
            "http://host:8000/videos/..%2Fsecrets.mp4"
        );
        assert_eq!(
            s.video_url("100%.mp4"),
            "http://host:8000/videos/100%25.mp4"
        );
    }

    #[tokio::test]
    async fn broadcast_with_no_subscribers_is_not_an_error() {
        let s = state("http://host:8000");
        // Would return Err from the raw channel; must be swallowed.
        s.broadcast(SseKind::VideoLibraryChanged, &serde_json::json!({}));
    }

    #[tokio::test]
    async fn subscribers_receive_broadcast_events() {
        let s = state("http://host:8000");
        let mut rx = s.subscribe();

        let device = Device {
            id: uuid::Uuid::new_v4(),
            name: "TV-01".to_string(),
            ip: "192.168.1.11".to_string(),
            state: shared::DeviceState::Playing,
            current_video: Some("clip.mp4".to_string()),
            last_seen: 1234,
        };
        s.broadcast(SseKind::DeviceUpdated, &device);

        let event = rx.try_recv().expect("event delivered");
        assert!(matches!(event.kind, SseKind::DeviceUpdated));
        assert_eq!(event.payload["name"], "TV-01");
        assert_eq!(event.payload["state"], "Playing");
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_event() {
        let s = state("http://host:8000");
        let mut a = s.subscribe();
        let mut b = s.subscribe();

        s.broadcast(SseKind::VideoLibraryChanged, &serde_json::json!({ "n": 1 }));

        assert_eq!(a.try_recv().unwrap().payload["n"], 1);
        assert_eq!(b.try_recv().unwrap().payload["n"], 1);
    }
}
