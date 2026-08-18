//! `GET /api/events` — the live event stream the dashboard subscribes to.
//!
//! Every state change in the server (registration, playback commands, the
//! heartbeat, the video scanner) is published into `AppState`'s broadcast
//! channel; this turns one subscriber's end of that channel into an SSE
//! response.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{Stream, StreamExt};
use shared::SseEvent;
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

use crate::state::AppState;

pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::debug!("dashboard subscribed to /api/events");

    // KeepAlive stops an idle wall of TVs from letting the connection be
    // dropped by a proxy or an aggressive NAT table.
    Sse::new(event_stream(state.subscribe())).keep_alive(KeepAlive::default())
}

/// Turn a broadcast receiver into a stream of SSE frames.
///
/// Split out from the handler so the lag and serialization paths can be tested
/// without an HTTP client.
fn event_stream(
    rx: broadcast::Receiver<SseEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx).filter_map(|message| async move {
        match message {
            Ok(event) => match Event::default().json_data(&event) {
                Ok(frame) => Some(Ok(frame)),
                Err(err) => {
                    // Dropping one frame beats tearing down every dashboard's
                    // connection over a single unserializable payload.
                    tracing::error!(%err, "failed to serialize SSE event; dropping it");
                    None
                }
            },

            // The subscriber fell more than the channel capacity behind and
            // those events are gone for good. The plan's sketch unwrapped here,
            // which would panic and kill the connection precisely when a
            // dashboard is already struggling.
            //
            // A named `lagged` frame lets a client resync by refetching. The
            // current dashboard hook only listens to unnamed messages, so it
            // ignores this — which is why the warning also goes to the log.
            Err(BroadcastStreamRecvError::Lagged(dropped)) => {
                tracing::warn!(dropped, "SSE subscriber lagged; events were skipped");
                Some(Ok(Event::default()
                    .event("lagged")
                    .data(dropped.to_string())))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::app;
    use crate::state::AppState;
    use shared::{Device, DeviceState, SseKind};
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::time::Duration;
    use uuid::Uuid;

    fn device(name: &str) -> Device {
        Device {
            id: Uuid::new_v4(),
            name: name.to_string(),
            ip: "10.0.0.1".to_string(),
            state: DeviceState::Playing,
            current_video: Some("clip.mp4".to_string()),
            last_seen: 42,
        }
    }

    fn an_event(name: &str) -> SseEvent {
        SseEvent {
            kind: SseKind::DeviceUpdated,
            payload: serde_json::to_value(device(name)).unwrap(),
        }
    }

    // ── Stream behaviour ────────────────────────────────────────────────────

    #[tokio::test]
    async fn events_become_frames_in_order() {
        let (tx, rx) = broadcast::channel(16);
        let mut stream = Box::pin(event_stream(rx));

        tx.send(an_event("TV-01")).unwrap();
        tx.send(an_event("TV-02")).unwrap();

        for expected in ["TV-01", "TV-02"] {
            let frame = stream.next().await.unwrap().unwrap();
            let rendered = format!("{frame:?}");
            assert!(
                rendered.contains(expected),
                "expected {expected} in {rendered}"
            );
        }
    }

    /// The case the plan's `msg.unwrap()` would have panicked on.
    #[tokio::test]
    async fn a_lagged_subscriber_gets_a_marker_instead_of_a_panic() {
        let (tx, rx) = broadcast::channel(4);
        let mut stream = Box::pin(event_stream(rx));

        // Overrun the buffer before the stream is ever polled.
        for i in 0..20 {
            tx.send(an_event(&format!("TV-{i:02}"))).unwrap();
        }

        let frame = stream.next().await.unwrap().unwrap();
        let rendered = format!("{frame:?}");
        assert!(
            rendered.contains("lagged"),
            "expected a lagged marker, got {rendered}"
        );

        // The stream keeps working rather than ending. After a lag the
        // receiver resumes at the oldest message still buffered, so the last
        // few of the overrun batch arrive before anything new.
        tx.send(an_event("TV-LATER")).unwrap();
        let mut seen = Vec::new();
        for _ in 0..10 {
            let frame = format!("{:?}", stream.next().await.unwrap().unwrap());
            let arrived = frame.contains("TV-LATER");
            seen.push(frame);
            if arrived {
                break;
            }
        }
        assert!(
            seen.last().is_some_and(|f| f.contains("TV-LATER")),
            "stream stalled after lagging; saw {seen:#?}"
        );
    }

    #[tokio::test]
    async fn the_stream_ends_when_the_sender_is_dropped() {
        let (tx, rx) = broadcast::channel(4);
        let mut stream = Box::pin(event_stream(rx));
        drop(tx);
        assert!(stream.next().await.is_none());
    }

    // ── Over HTTP ───────────────────────────────────────────────────────────

    struct Harness {
        root: PathBuf,
        base: String,
        state: Arc<AppState>,
        client: reqwest::Client,
    }

    impl Harness {
        async fn new() -> Harness {
            let root = std::env::temp_dir().join(format!("tv-controller-sse-{}", Uuid::new_v4()));
            let videos = root.join("videos");
            std::fs::create_dir_all(&videos).unwrap();
            let db = crate::db::connect(&format!("sqlite:{}", root.join("test.db").display()))
                .await
                .unwrap();
            let state = AppState::new(db, "http://host:8000", videos, reqwest::Client::new());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            let router = app(state.clone(), None);
            tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });

            Harness {
                root,
                base: format!("http://{addr}"),
                state,
                client: reqwest::Client::new(),
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Read from the response until a complete `data:` frame arrives.
    async fn next_data_frame(resp: &mut reqwest::Response) -> String {
        let mut buffer = String::new();
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
                .await
                .expect("timed out waiting for an SSE frame")
                .unwrap()
                .expect("stream ended early");
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            if let Some(line) = buffer.lines().find(|l| l.starts_with("data:")) {
                if buffer.contains("\n\n") {
                    return line.trim_start_matches("data:").trim().to_string();
                }
            }
        }
    }

    #[tokio::test]
    async fn the_endpoint_serves_an_event_stream() {
        let h = Harness::new().await;
        let resp = h
            .client
            .get(format!("{}/api/events", h.base))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }

    #[tokio::test]
    async fn a_broadcast_reaches_a_connected_client() {
        let h = Harness::new().await;
        let mut resp = h
            .client
            .get(format!("{}/api/events", h.base))
            .send()
            .await
            .unwrap();

        // Give the handler a moment to subscribe before publishing, otherwise
        // the event is sent to nobody.
        tokio::time::sleep(Duration::from_millis(100)).await;
        h.state.broadcast(SseKind::DeviceUpdated, &device("TV-01"));

        let data = next_data_frame(&mut resp).await;
        let event: SseEvent = serde_json::from_str(&data).expect("frame should be an SseEvent");
        assert!(matches!(event.kind, SseKind::DeviceUpdated));
        assert_eq!(event.payload["name"], "TV-01");
    }

    #[tokio::test]
    async fn every_connected_dashboard_receives_the_same_event() {
        let h = Harness::new().await;
        let mut first = h
            .client
            .get(format!("{}/api/events", h.base))
            .send()
            .await
            .unwrap();
        let mut second = h
            .client
            .get(format!("{}/api/events", h.base))
            .send()
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        h.state
            .broadcast(SseKind::VideoLibraryChanged, &serde_json::json!({ "n": 1 }));

        for resp in [&mut first, &mut second] {
            let data = next_data_frame(resp).await;
            let event: SseEvent = serde_json::from_str(&data).unwrap();
            assert!(matches!(event.kind, SseKind::VideoLibraryChanged));
            assert_eq!(event.payload["n"], 1);
        }
    }

    /// A device registration is broadcast by its handler; this checks the whole
    /// path from an API call to a frame on the wire.
    #[tokio::test]
    async fn registering_a_device_shows_up_on_the_stream() {
        let h = Harness::new().await;
        let mut resp = h
            .client
            .get(format!("{}/api/events", h.base))
            .send()
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        h.client
            .post(format!("{}/api/devices/register", h.base))
            .json(&serde_json::json!({
                "id": Uuid::new_v4(),
                "name": "TV-NEW",
                "ip": "192.168.1.50",
            }))
            .send()
            .await
            .unwrap();

        let data = next_data_frame(&mut resp).await;
        let event: SseEvent = serde_json::from_str(&data).unwrap();
        assert!(matches!(event.kind, SseKind::DeviceUpdated));
        assert_eq!(event.payload["name"], "TV-NEW");
    }
}
