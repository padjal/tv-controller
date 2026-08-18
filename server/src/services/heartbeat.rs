//! Background task that polls every agent for its playback state.
//!
//! This is the server's only source of truth about what a TV is *actually*
//! doing. Playback handlers record what they asked for; the heartbeat records
//! what happened — so a Pi that rebooted mid-playback, or that was stopped at
//! the TV itself, converges back to reality within one poll.
//!
//! Note: `run` is spawned from `main`, but nothing calls `poll_once` outside
//! tests yet, hence the module-level `dead_code` allow.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::future::join_all;
use shared::{AgentStatus, Device, DeviceState, SseKind};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use super::agent_base_url;
use crate::db;
use crate::state::AppState;

/// How often every device is polled.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// A device unreachable for longer than this is marked Offline. The grace
/// period spans a few missed polls, so one dropped packet or a brief Wi-Fi
/// stall does not flash the dashboard red.
pub const OFFLINE_AFTER: Duration = Duration::from_secs(30);

/// Tighter than the playback timeout: this runs every 10s, so a slow agent
/// must not still be hanging when the next tick arrives.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

/// HTTP client for status polls, with [`STATUS_TIMEOUT`] applied.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(STATUS_TIMEOUT)
        .build()
        .context("failed to build HTTP client for heartbeat")
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PollSummary {
    pub polled: usize,
    pub reachable: usize,
    /// Devices that crossed the grace period and were newly marked Offline.
    pub marked_offline: usize,
    /// Devices whose state or current video actually changed, and so were
    /// broadcast to the dashboard.
    pub changed: usize,
}

/// Poll every device forever. Spawned once at startup.
pub async fn run(state: Arc<AppState>, client: reqwest::Client) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    // A poll that overruns its slot must not cause a burst of catch-up ticks.
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        match poll_once(&state, &client).await {
            Ok(summary) if summary.changed > 0 || summary.marked_offline > 0 => {
                tracing::info!(?summary, "heartbeat");
            }
            Ok(summary) => tracing::debug!(?summary, "heartbeat"),
            // A failed round (usually the database) must not kill the task.
            Err(err) => tracing::error!(%err, "heartbeat round failed"),
        }
    }
}

/// One round: poll all devices concurrently and reconcile the database.
pub async fn poll_once(state: &AppState, client: &reqwest::Client) -> Result<PollSummary> {
    let devices = db::list_devices(&state.db).await?;
    if devices.is_empty() {
        return Ok(PollSummary::default());
    }

    // Concurrent, not sequential: polling twenty devices one at a time at up to
    // STATUS_TIMEOUT each would overrun the interval and leave the last device
    // minutes stale.
    let polls = devices
        .iter()
        .map(|device| async move { (device, fetch_status(device, client).await) });
    let results = join_all(polls).await;

    let filenames = video_filenames_if_needed(state, &results).await?;

    let now = current_unix();
    let mut summary = PollSummary {
        polled: results.len(),
        ..Default::default()
    };

    for (device, result) in results {
        match result {
            Ok(status) => {
                summary.reachable += 1;
                let current_video = status
                    .current_video_id
                    .and_then(|id| filenames.get(&id).cloned());
                apply(
                    state,
                    device,
                    &status.state,
                    current_video.as_deref(),
                    now,
                    &mut summary,
                )
                .await?;
            }
            Err(err) => {
                let silent_for = now.saturating_sub(device.last_seen);
                if silent_for < OFFLINE_AFTER.as_secs() as i64 {
                    // Within the grace period: leave the row alone, including
                    // last_seen, so the clock keeps running toward Offline.
                    tracing::debug!(
                        device = %device.name, %err, silent_for,
                        "agent poll failed, still within grace period"
                    );
                    continue;
                }

                if device.state == DeviceState::Offline {
                    // Already known to be gone; nothing to say.
                    continue;
                }

                tracing::warn!(device = %device.name, %err, silent_for, "marking device offline");
                summary.marked_offline += 1;
                mark_offline(state, device).await?;
            }
        }
    }

    Ok(summary)
}

/// Persist a reachable device's reported state, broadcasting only if it moved.
async fn apply(
    state: &AppState,
    device: &Device,
    reported: &DeviceState,
    current_video: Option<&str>,
    now: i64,
    summary: &mut PollSummary,
) -> Result<()> {
    // last_seen is refreshed every round, but a heartbeat that only moved the
    // clock is not worth waking every dashboard 6 times a minute per device.
    let moved = device.state != *reported || device.current_video.as_deref() != current_video;

    db::update_device_state(&state.db, device.id, reported, current_video, now).await?;

    if moved {
        summary.changed += 1;
        state.broadcast(
            SseKind::DeviceUpdated,
            &Device {
                state: reported.clone(),
                current_video: current_video.map(str::to_owned),
                last_seen: now,
                ..device.clone()
            },
        );
    }

    Ok(())
}

async fn mark_offline(state: &AppState, device: &Device) -> Result<()> {
    // last_seen is deliberately left at its old value — it records when the
    // device was last actually *seen*, which is what the grace period measures.
    // current_video is cleared: whatever it was playing, we no longer know.
    db::update_device_state(
        &state.db,
        device.id,
        &DeviceState::Offline,
        None,
        device.last_seen,
    )
    .await?;

    state.broadcast(
        SseKind::DeviceOffline,
        &Device {
            state: DeviceState::Offline,
            current_video: None,
            ..device.clone()
        },
    );
    Ok(())
}

/// Map video id → filename, but only if some agent actually reported a video.
///
/// `AgentStatus` carries a video id while `devices.current_video` holds the
/// filename the dashboard displays, so the ids have to be resolved. One query
/// per round covers every device.
async fn video_filenames_if_needed(
    state: &AppState,
    results: &[(&Device, Result<AgentStatus>)],
) -> Result<HashMap<Uuid, String>> {
    let any_video = results
        .iter()
        .any(|(_, r)| matches!(r, Ok(s) if s.current_video_id.is_some()));
    if !any_video {
        return Ok(HashMap::new());
    }

    Ok(db::list_videos(&state.db)
        .await?
        .into_iter()
        .map(|v| (v.id, v.filename))
        .collect())
}

async fn fetch_status(device: &Device, client: &reqwest::Client) -> Result<AgentStatus> {
    let url = format!("{}/status", agent_base_url(&device.ip));

    let response =
        client.get(&url).send().await.with_context(|| {
            format!("{} ({}) did not answer GET /status", device.name, device.ip)
        })?;

    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "{} ({}) returned {status} for GET /status",
            device.name,
            device.ip
        ));
    }

    response
        .json::<AgentStatus>()
        .await
        .with_context(|| format!("{} ({}) sent an unreadable status", device.name, device.ip))
}

fn current_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use shared::RegisterRequest;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    struct Harness {
        root: PathBuf,
        state: Arc<AppState>,
    }

    impl Harness {
        async fn new() -> Harness {
            let root = std::env::temp_dir().join(format!("tv-controller-hb-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let db = db::connect(&format!("sqlite:{}", root.join("test.db").display()))
                .await
                .unwrap();
            let state = AppState::new(db, "http://host:8000", root.join("videos"));
            Harness { root, state }
        }

        /// Register a device and force its stored state/last_seen.
        async fn device(&self, name: &str, ip: &str, state: DeviceState, last_seen: i64) -> Uuid {
            let req = RegisterRequest {
                id: Uuid::new_v4(),
                name: name.to_string(),
                ip: ip.to_string(),
            };
            db::upsert_device(&self.state.db, &req).await.unwrap();
            db::update_device_state(&self.state.db, req.id, &state, None, last_seen)
                .await
                .unwrap();
            req.id
        }

        async fn get(&self, id: Uuid) -> Device {
            db::get_device(&self.state.db, id).await.unwrap().unwrap()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    async fn stub_agent(router: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    /// See the note in fan_out's tests: a proxy is what lets a stub on an
    /// ephemeral port answer a URL that always targets AGENT_PORT.
    fn client_via(addr: SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(STATUS_TIMEOUT)
            .proxy(reqwest::Proxy::all(format!("http://{addr}")).unwrap())
            .build()
            .unwrap()
    }

    /// A client that reaches nothing, quickly.
    fn dead_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .unwrap()
    }

    fn status_router(status: AgentStatus) -> Router {
        Router::new().route(
            "/status",
            get(move || {
                let status = AgentStatus {
                    state: status.state.clone(),
                    current_video_id: status.current_video_id,
                    position_secs: status.position_secs,
                    duration_secs: status.duration_secs,
                };
                async move { Json(status) }
            }),
        )
    }

    fn playing(video_id: Option<Uuid>) -> AgentStatus {
        AgentStatus {
            state: DeviceState::Playing,
            current_video_id: video_id,
            position_secs: Some(12.5),
            duration_secs: Some(60.0),
        }
    }

    #[tokio::test]
    async fn no_devices_is_a_no_op() {
        let h = Harness::new().await;
        let summary = poll_once(&h.state, &dead_client()).await.unwrap();
        assert_eq!(summary, PollSummary::default());
    }

    #[tokio::test]
    async fn a_reachable_device_has_its_state_recorded() {
        let h = Harness::new().await;
        let id = h.device("TV-01", "10.0.0.1", DeviceState::Idle, 0).await;
        let addr = stub_agent(status_router(playing(None))).await;

        let mut rx = h.state.subscribe();
        let summary = poll_once(&h.state, &client_via(addr)).await.unwrap();

        assert_eq!(summary.polled, 1);
        assert_eq!(summary.reachable, 1);
        assert_eq!(summary.changed, 1);
        assert_eq!(summary.marked_offline, 0);

        let device = h.get(id).await;
        assert_eq!(device.state, DeviceState::Playing);
        assert!(device.last_seen > 0, "last_seen should be refreshed");

        let event = rx.try_recv().expect("DeviceUpdated broadcast");
        assert!(matches!(event.kind, SseKind::DeviceUpdated));
        assert_eq!(event.payload["state"], "Playing");
    }

    #[tokio::test]
    async fn the_reported_video_id_is_resolved_to_a_filename() {
        let h = Harness::new().await;
        let id = h.device("TV-01", "10.0.0.1", DeviceState::Idle, 0).await;
        let video = db::upsert_video(&h.state.db, "clip.mp4", "/v/clip.mp4", 10, Some(60))
            .await
            .unwrap();

        let addr = stub_agent(status_router(playing(Some(video.id)))).await;
        poll_once(&h.state, &client_via(addr)).await.unwrap();

        let device = h.get(id).await;
        assert_eq!(
            device.current_video.as_deref(),
            Some("clip.mp4"),
            "dashboard shows a filename, agents report an id"
        );
    }

    #[tokio::test]
    async fn an_unchanged_device_is_not_rebroadcast() {
        let h = Harness::new().await;
        h.device("TV-01", "10.0.0.1", DeviceState::Idle, 0).await;
        let addr = stub_agent(status_router(AgentStatus {
            state: DeviceState::Idle,
            current_video_id: None,
            position_secs: None,
            duration_secs: None,
        }))
        .await;
        let client = client_via(addr);

        let mut rx = h.state.subscribe();
        let summary = poll_once(&h.state, &client).await.unwrap();

        assert_eq!(summary.reachable, 1);
        assert_eq!(summary.changed, 0, "state did not move, so nothing to send");
        assert!(
            rx.try_recv().is_err(),
            "a heartbeat that only moved the clock must not wake the dashboard"
        );
    }

    #[tokio::test]
    async fn a_briefly_unreachable_device_is_not_marked_offline() {
        let h = Harness::new().await;
        let recently = current_unix() - 5;
        let id = h
            .device("TV-01", "192.0.2.1", DeviceState::Playing, recently)
            .await;

        let mut rx = h.state.subscribe();
        let summary = poll_once(&h.state, &dead_client()).await.unwrap();

        assert_eq!(summary.reachable, 0);
        assert_eq!(summary.marked_offline, 0);
        let device = h.get(id).await;
        assert_eq!(
            device.state,
            DeviceState::Playing,
            "one missed poll inside the grace period must not flip the badge"
        );
        assert_eq!(
            device.last_seen, recently,
            "the offline clock keeps running"
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_long_silent_device_is_marked_offline_once() {
        let h = Harness::new().await;
        let long_ago = current_unix() - 120;
        let id = h
            .device("TV-01", "192.0.2.1", DeviceState::Playing, long_ago)
            .await;

        let mut rx = h.state.subscribe();
        let summary = poll_once(&h.state, &dead_client()).await.unwrap();

        assert_eq!(summary.marked_offline, 1);
        let device = h.get(id).await;
        assert_eq!(device.state, DeviceState::Offline);
        assert_eq!(device.current_video, None);

        let event = rx.try_recv().expect("DeviceOffline broadcast");
        assert!(matches!(event.kind, SseKind::DeviceOffline));

        // A second round must not re-announce a device already known offline.
        let summary = poll_once(&h.state, &dead_client()).await.unwrap();
        assert_eq!(summary.marked_offline, 0);
        assert!(rx.try_recv().is_err(), "offline must be announced once");
    }

    #[tokio::test]
    async fn a_device_that_comes_back_is_picked_up_again() {
        let h = Harness::new().await;
        let id = h
            .device(
                "TV-01",
                "10.0.0.1",
                DeviceState::Offline,
                current_unix() - 120,
            )
            .await;

        let addr = stub_agent(status_router(playing(None))).await;
        let summary = poll_once(&h.state, &client_via(addr)).await.unwrap();

        assert_eq!(summary.reachable, 1);
        assert_eq!(summary.changed, 1);
        assert_eq!(h.get(id).await.state, DeviceState::Playing);
    }

    /// The case left open in Task 3.1: registration preserves playback state,
    /// so a Pi that reboots mid-playback stays "Playing" in the database until
    /// the heartbeat sees it report Idle.
    #[tokio::test]
    async fn a_rebooted_pi_reporting_idle_clears_the_stale_playing_state() {
        let h = Harness::new().await;
        let id = h.device("TV-01", "10.0.0.1", DeviceState::Playing, 0).await;
        db::update_device_state(
            &h.state.db,
            id,
            &DeviceState::Playing,
            Some("clip.mp4"),
            current_unix(),
        )
        .await
        .unwrap();

        let addr = stub_agent(status_router(AgentStatus {
            state: DeviceState::Idle,
            current_video_id: None,
            position_secs: None,
            duration_secs: None,
        }))
        .await;
        let summary = poll_once(&h.state, &client_via(addr)).await.unwrap();

        assert_eq!(summary.changed, 1);
        let device = h.get(id).await;
        assert_eq!(device.state, DeviceState::Idle);
        assert_eq!(device.current_video, None);
    }

    #[tokio::test]
    async fn one_dead_device_does_not_stop_the_others_being_polled() {
        let h = Harness::new().await;
        // Reachable through the proxy.
        let good = h.device("TV-GOOD", "10.0.0.1", DeviceState::Idle, 0).await;
        // Sorted after TV-GOOD by name, so it is handled in the same round.
        let bad = h
            .device(
                "TV-ZOMBIE",
                "10.0.0.2",
                DeviceState::Idle,
                current_unix() - 120,
            )
            .await;

        // The stub answers /status for everyone; make the zombie fail by
        // routing only its host to a 500.
        let addr = stub_agent(Router::new().route(
            "/status",
            get(|headers: axum::http::HeaderMap| async move {
                let host = headers
                    .get(axum::http::header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                if host.starts_with("10.0.0.2") {
                    axum::response::IntoResponse::into_response((
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "mpv gone",
                    ))
                } else {
                    axum::response::IntoResponse::into_response(Json(playing(None)))
                }
            }),
        ))
        .await;

        let summary = poll_once(&h.state, &client_via(addr)).await.unwrap();

        assert_eq!(summary.polled, 2);
        assert_eq!(summary.reachable, 1);
        assert_eq!(summary.marked_offline, 1);
        assert_eq!(h.get(good).await.state, DeviceState::Playing);
        assert_eq!(h.get(bad).await.state, DeviceState::Offline);
    }

    #[tokio::test]
    async fn devices_are_polled_concurrently() {
        const DELAY: Duration = Duration::from_millis(300);
        let h = Harness::new().await;
        for i in 0..5 {
            h.device(
                &format!("TV-0{i}"),
                &format!("10.0.0.{i}"),
                DeviceState::Idle,
                0,
            )
            .await;
        }

        let addr = stub_agent(Router::new().route(
            "/status",
            get(|| async {
                tokio::time::sleep(DELAY).await;
                Json(playing(None))
            }),
        ))
        .await;

        let started = std::time::Instant::now();
        let summary = poll_once(&h.state, &client_via(addr)).await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(summary.reachable, 5);
        assert!(
            elapsed < DELAY * 3,
            "polling 5 devices took {elapsed:?}; they are not concurrent"
        );
    }
}
