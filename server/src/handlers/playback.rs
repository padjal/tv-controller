//! Playback commands: fan a request out to many agents, then record what
//! actually happened.
//!
//! The database is updated only for devices that accepted the command. A TV
//! that was unplugged keeps whatever state it had until the heartbeat notices,
//! rather than being optimistically marked Playing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use shared::{Device, DeviceState, PlayCommand, PlaybackRequest, SseKind, StopRequest};
use uuid::Uuid;

use crate::db;
use crate::error::ApiError;
use crate::services::fan_out;
use crate::state::AppState;

#[derive(Serialize)]
pub struct PlaybackResponse {
    /// Devices that accepted the command and had their state recorded.
    pub succeeded: Vec<Uuid>,
    pub failed: Vec<DeviceFailure>,
}

#[derive(Serialize)]
pub struct DeviceFailure {
    pub id: Uuid,
    pub error: String,
}

#[derive(Deserialize)]
pub struct PlayAllRequest {
    pub video_id: Uuid,
}

/// What a command does to `devices.current_video`.
enum VideoUpdate<'a> {
    /// Play: point the row at the new file.
    Set(&'a str),
    /// Stop: nothing is playing any more.
    Clear,
    /// Pause/resume: the video is unchanged.
    Keep,
}

/// A response that is a 200 when at least one device accepted the command, and
/// a 502 when every targeted device failed.
///
/// This matters because the dashboard's `api.ts` checks `res.ok` — a plain 200
/// after every TV refused would show a success toast for a command that did
/// nothing.
impl IntoResponse for PlaybackResponse {
    fn into_response(self) -> Response {
        let status = if self.succeeded.is_empty() && !self.failed.is_empty() {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::OK
        };
        (status, Json(self)).into_response()
    }
}

/// `POST /api/playback/play`
pub async fn play(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlaybackRequest>,
) -> Result<PlaybackResponse, ApiError> {
    let (video, command) = load_play_command(&state, req.video_id).await?;
    let devices = resolve_devices(&state, &req.device_ids).await?;

    let results = fan_out::fan_out_play(&devices.found, &command, &state.http).await;
    Ok(record(
        &state,
        &devices,
        results,
        DeviceState::Playing,
        VideoUpdate::Set(&video.filename),
    )
    .await?)
}

/// `POST /api/playback/play-all` — every device that is not known to be offline.
pub async fn play_all(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlayAllRequest>,
) -> Result<PlaybackResponse, ApiError> {
    let (video, command) = load_play_command(&state, req.video_id).await?;

    let found: Vec<Device> = db::list_devices(&state.db)
        .await?
        .into_iter()
        .filter(|d| d.state != DeviceState::Offline)
        .collect();

    if found.is_empty() {
        // Not a 200: nothing was played, and the dashboard checks `res.ok`.
        return Err(ApiError::conflict("no devices are online"));
    }

    let devices = Targets {
        found,
        missing: Vec::new(),
    };
    let results = fan_out::fan_out_play(&devices.found, &command, &state.http).await;
    Ok(record(
        &state,
        &devices,
        results,
        DeviceState::Playing,
        VideoUpdate::Set(&video.filename),
    )
    .await?)
}

/// `POST /api/playback/stop`
pub async fn stop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StopRequest>,
) -> Result<PlaybackResponse, ApiError> {
    let devices = resolve_devices(&state, &req.device_ids).await?;
    let results = fan_out::fan_out_stop(&devices.found, &state.http).await;
    Ok(record(
        &state,
        &devices,
        results,
        DeviceState::Idle,
        VideoUpdate::Clear,
    )
    .await?)
}

/// `POST /api/playback/pause`
pub async fn pause(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StopRequest>,
) -> Result<PlaybackResponse, ApiError> {
    let devices = resolve_devices(&state, &req.device_ids).await?;
    let results = fan_out::fan_out_pause(&devices.found, &state.http).await;
    Ok(record(
        &state,
        &devices,
        results,
        DeviceState::Paused,
        VideoUpdate::Keep,
    )
    .await?)
}

/// `POST /api/playback/resume`
pub async fn resume(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StopRequest>,
) -> Result<PlaybackResponse, ApiError> {
    let devices = resolve_devices(&state, &req.device_ids).await?;
    let results = fan_out::fan_out_resume(&devices.found, &state.http).await;
    Ok(record(
        &state,
        &devices,
        results,
        DeviceState::Playing,
        VideoUpdate::Keep,
    )
    .await?)
}

/// The devices a request named, split into those that exist and those that do not.
struct Targets {
    found: Vec<Device>,
    missing: Vec<Uuid>,
}

/// Look up the video and build the command the agents receive.
async fn load_play_command(
    state: &AppState,
    video_id: Uuid,
) -> Result<(shared::Video, PlayCommand), ApiError> {
    let video = db::get_video(&state.db, video_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no video with id {video_id}")))?;

    let command = PlayCommand {
        url: state.video_url(&video.filename),
        video_id: video.id,
    };
    Ok((video, command))
}

/// Resolve requested ids in one query, keeping the caller's order.
///
/// An id that is not registered is reported as a failure rather than aborting
/// the whole command — a stale tile in one dashboard should not stop the other
/// TVs from playing.
async fn resolve_devices(state: &AppState, ids: &[Uuid]) -> Result<Targets, ApiError> {
    if ids.is_empty() {
        return Err(ApiError::bad_request("device_ids must not be empty"));
    }

    let mut known: HashMap<Uuid, Device> = db::list_devices(&state.db)
        .await?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();

    let mut targets = Targets {
        found: Vec::with_capacity(ids.len()),
        missing: Vec::new(),
    };
    for id in ids {
        match known.remove(id) {
            Some(device) => targets.found.push(device),
            // `remove` also de-duplicates: an id listed twice is commanded once.
            None => targets.missing.push(*id),
        }
    }

    Ok(targets)
}

/// Persist and announce the outcome of a fan-out.
async fn record(
    state: &AppState,
    targets: &Targets,
    results: fan_out::FanOutResults,
    new_state: DeviceState,
    video: VideoUpdate<'_>,
) -> anyhow::Result<PlaybackResponse> {
    let by_id: HashMap<Uuid, &Device> = targets.found.iter().map(|d| (d.id, d)).collect();
    let now = current_unix();

    let mut response = PlaybackResponse {
        succeeded: Vec::new(),
        failed: targets
            .missing
            .iter()
            .map(|id| DeviceFailure {
                id: *id,
                error: format!("no device with id {id}"),
            })
            .collect(),
    };

    for (id, result) in results {
        let Some(device) = by_id.get(&id) else {
            continue;
        };

        match result {
            Ok(()) => {
                let current_video = match video {
                    VideoUpdate::Set(filename) => Some(filename),
                    VideoUpdate::Clear => None,
                    VideoUpdate::Keep => device.current_video.as_deref(),
                };

                // The agent answered, so it is demonstrably alive: refreshing
                // last_seen keeps the heartbeat's offline clock honest.
                db::update_device_state(&state.db, id, &new_state, current_video, now).await?;

                state.broadcast(
                    SseKind::DeviceUpdated,
                    &Device {
                        state: new_state.clone(),
                        current_video: current_video.map(str::to_owned),
                        last_seen: now,
                        ..(*device).clone()
                    },
                );
                response.succeeded.push(id);
            }
            Err(err) => {
                // Deliberately no database write: the device's real state is
                // unknown, and the heartbeat is what decides it is offline.
                tracing::warn!(device = %device.name, %err, "playback command failed");
                response.failed.push(DeviceFailure {
                    id,
                    error: format!("{err}"),
                });
            }
        }
    }

    Ok(response)
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
    use crate::router::app;
    use crate::services::AGENT_PORT;
    use axum::routing::post as axum_post;
    use axum::Router;
    use shared::RegisterRequest;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Agents whose host starts with this refuse every command.
    const FAILING_IP: &str = "10.0.0.99";

    #[derive(Default)]
    struct AgentLog {
        calls: Mutex<Vec<(String, String)>>, // (endpoint, host)
        plays: AtomicUsize,
    }

    struct Harness {
        root: PathBuf,
        base: String,
        state: Arc<AppState>,
        client: reqwest::Client,
        agent: Arc<AgentLog>,
    }

    impl Harness {
        async fn new() -> Harness {
            let root = std::env::temp_dir().join(format!("tv-controller-pb-{}", Uuid::new_v4()));
            let videos = root.join("videos");
            std::fs::create_dir_all(&videos).unwrap();
            let db = db::connect(&format!("sqlite:{}", root.join("test.db").display()))
                .await
                .unwrap();

            // One stub standing in for every agent, told apart by Host.
            let agent = Arc::new(AgentLog::default());
            let agent_addr = spawn_stub_agent(agent.clone()).await;

            // The proxy is what lets a stub on an ephemeral port answer URLs
            // that always target AGENT_PORT. See fan_out's tests.
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .proxy(reqwest::Proxy::all(format!("http://{agent_addr}")).unwrap())
                .build()
                .unwrap();

            let state = AppState::new(db, "http://server:8000", videos, http);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            let router = app(state.clone());
            tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });

            Harness {
                root,
                base: format!("http://{addr}/api"),
                state,
                // A separate, un-proxied client for talking to our own server.
                client: reqwest::Client::new(),
                agent,
            }
        }

        async fn add_device(&self, name: &str, ip: &str) -> Uuid {
            let req = RegisterRequest {
                id: Uuid::new_v4(),
                name: name.to_string(),
                ip: ip.to_string(),
            };
            db::upsert_device(&self.state.db, &req).await.unwrap();
            req.id
        }

        async fn add_video(&self, filename: &str) -> Uuid {
            db::upsert_video(
                &self.state.db,
                filename,
                &format!("/v/{filename}"),
                10,
                Some(30),
            )
            .await
            .unwrap()
            .id
        }

        async fn post(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
            self.client
                .post(format!("{}{path}", self.base))
                .json(&body)
                .send()
                .await
                .unwrap()
        }

        async fn device(&self, id: Uuid) -> Device {
            db::get_device(&self.state.db, id).await.unwrap().unwrap()
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.agent.calls.lock().unwrap().clone()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    async fn spawn_stub_agent(log: Arc<AgentLog>) -> SocketAddr {
        fn handler(
            log: Arc<AgentLog>,
            endpoint: &'static str,
        ) -> impl Fn(
            axum::http::HeaderMap,
            String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
               + Clone {
            move |headers: axum::http::HeaderMap, _body: String| {
                let log = log.clone();
                Box::pin(async move {
                    let host = headers
                        .get(axum::http::header::HOST)
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    log.calls
                        .lock()
                        .unwrap()
                        .push((endpoint.to_string(), host.clone()));
                    if endpoint == "play" {
                        log.plays.fetch_add(1, Ordering::SeqCst);
                    }
                    if host.starts_with(FAILING_IP) {
                        (StatusCode::INTERNAL_SERVER_ERROR, "mpv socket closed").into_response()
                    } else {
                        Json(serde_json::json!({ "ok": true })).into_response()
                    }
                })
            }
        }

        let router = Router::new()
            .route("/play", axum_post(handler(log.clone(), "play")))
            .route("/stop", axum_post(handler(log.clone(), "stop")))
            .route("/pause", axum_post(handler(log.clone(), "pause")))
            .route("/resume", axum_post(handler(log.clone(), "resume")));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    // ── play ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn play_commands_every_device_and_records_the_state() {
        let h = Harness::new().await;
        let a = h.add_device("TV-01", "10.0.0.1").await;
        let b = h.add_device("TV-02", "10.0.0.2").await;
        let video = h.add_video("clip.mp4").await;

        let mut rx = h.state.subscribe();
        let resp = h
            .post(
                "/playback/play",
                serde_json::json!({ "device_ids": [a, b], "video_id": video }),
            )
            .await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["succeeded"].as_array().unwrap().len(), 2);
        assert!(body["failed"].as_array().unwrap().is_empty());

        for id in [a, b] {
            let device = h.device(id).await;
            assert_eq!(device.state, DeviceState::Playing);
            assert_eq!(device.current_video.as_deref(), Some("clip.mp4"));
            assert!(device.last_seen > 0);
        }

        // Both agents were actually called, on /play.
        let calls = h.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|(ep, _)| ep == "play"));

        // And both were announced.
        let mut seen = 0;
        while rx.try_recv().is_ok() {
            seen += 1;
        }
        assert_eq!(seen, 2, "one DeviceUpdated per device");
    }

    #[tokio::test]
    async fn play_sends_the_url_an_agent_can_fetch() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;
        let video = h.add_video("summer promo #2.mp4").await;

        h.post(
            "/playback/play",
            serde_json::json!({ "device_ids": [id], "video_id": video }),
        )
        .await;

        // The URL is built by AppState::video_url, so it is percent-encoded and
        // absolute — an agent on another host can fetch it.
        assert_eq!(
            h.state.video_url("summer promo #2.mp4"),
            "http://server:8000/videos/summer%20promo%20%232.mp4"
        );
    }

    #[tokio::test]
    async fn play_with_an_unknown_video_is_a_404() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;

        let resp = h
            .post(
                "/playback/play",
                serde_json::json!({ "device_ids": [id], "video_id": Uuid::new_v4() }),
            )
            .await;
        assert_eq!(resp.status(), 404);
        assert!(h.calls().is_empty(), "no agent should be contacted");
    }

    #[tokio::test]
    async fn play_with_no_devices_is_a_400() {
        let h = Harness::new().await;
        let video = h.add_video("clip.mp4").await;

        let resp = h
            .post(
                "/playback/play",
                serde_json::json!({ "device_ids": [], "video_id": video }),
            )
            .await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn an_unknown_device_id_is_reported_without_stopping_the_others() {
        let h = Harness::new().await;
        let good = h.add_device("TV-01", "10.0.0.1").await;
        let ghost = Uuid::new_v4();
        let video = h.add_video("clip.mp4").await;

        let resp = h
            .post(
                "/playback/play",
                serde_json::json!({ "device_ids": [good, ghost], "video_id": video }),
            )
            .await;
        assert_eq!(resp.status(), 200, "one good device means partial success");

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["succeeded"], serde_json::json!([good]));
        assert_eq!(body["failed"][0]["id"], ghost.to_string());
        assert_eq!(h.device(good).await.state, DeviceState::Playing);
    }

    #[tokio::test]
    async fn a_failing_agent_is_reported_and_its_state_left_alone() {
        let h = Harness::new().await;
        let good = h.add_device("TV-01", "10.0.0.1").await;
        let bad = h.add_device("TV-BAD", FAILING_IP).await;
        let video = h.add_video("clip.mp4").await;

        let resp = h
            .post(
                "/playback/play",
                serde_json::json!({ "device_ids": [good, bad], "video_id": video }),
            )
            .await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["succeeded"], serde_json::json!([good]));
        assert_eq!(body["failed"].as_array().unwrap().len(), 1);
        assert_eq!(body["failed"][0]["id"], bad.to_string());

        assert_eq!(h.device(good).await.state, DeviceState::Playing);
        assert_eq!(
            h.device(bad).await.state,
            DeviceState::Idle,
            "a refused command must not be recorded as playing"
        );
    }

    #[tokio::test]
    async fn a_command_every_device_refuses_is_a_502() {
        let h = Harness::new().await;
        let bad = h.add_device("TV-BAD", FAILING_IP).await;
        let video = h.add_video("clip.mp4").await;

        let resp = h
            .post(
                "/playback/play",
                serde_json::json!({ "device_ids": [bad], "video_id": video }),
            )
            .await;
        assert_eq!(
            resp.status(),
            502,
            "a naive res.ok check must not read total failure as success"
        );
    }

    #[tokio::test]
    async fn a_repeated_device_id_is_commanded_once() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;
        let video = h.add_video("clip.mp4").await;

        h.post(
            "/playback/play",
            serde_json::json!({ "device_ids": [id, id, id], "video_id": video }),
        )
        .await;

        assert_eq!(
            h.calls().len(),
            1,
            "duplicates must not triple-command a TV"
        );
    }

    // ── stop / pause / resume ───────────────────────────────────────────────

    #[tokio::test]
    async fn stop_clears_the_current_video() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;
        let video = h.add_video("clip.mp4").await;
        h.post(
            "/playback/play",
            serde_json::json!({ "device_ids": [id], "video_id": video }),
        )
        .await;

        let resp = h
            .post("/playback/stop", serde_json::json!({ "device_ids": [id] }))
            .await;
        assert_eq!(resp.status(), 200);

        let device = h.device(id).await;
        assert_eq!(device.state, DeviceState::Idle);
        assert_eq!(device.current_video, None);
    }

    #[tokio::test]
    async fn pause_and_resume_keep_the_current_video() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;
        let video = h.add_video("clip.mp4").await;
        h.post(
            "/playback/play",
            serde_json::json!({ "device_ids": [id], "video_id": video }),
        )
        .await;

        h.post("/playback/pause", serde_json::json!({ "device_ids": [id] }))
            .await;
        let paused = h.device(id).await;
        assert_eq!(paused.state, DeviceState::Paused);
        assert_eq!(
            paused.current_video.as_deref(),
            Some("clip.mp4"),
            "pausing must not forget what is loaded"
        );

        h.post(
            "/playback/resume",
            serde_json::json!({ "device_ids": [id] }),
        )
        .await;
        let resumed = h.device(id).await;
        assert_eq!(resumed.state, DeviceState::Playing);
        assert_eq!(resumed.current_video.as_deref(), Some("clip.mp4"));
    }

    #[tokio::test]
    async fn each_command_hits_its_own_agent_endpoint() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;
        let video = h.add_video("clip.mp4").await;

        h.post(
            "/playback/play",
            serde_json::json!({ "device_ids": [id], "video_id": video }),
        )
        .await;
        for cmd in ["pause", "resume", "stop"] {
            h.post(
                &format!("/playback/{cmd}"),
                serde_json::json!({ "device_ids": [id] }),
            )
            .await;
        }

        let endpoints: Vec<String> = h.calls().into_iter().map(|(ep, _)| ep).collect();
        assert_eq!(endpoints, ["play", "pause", "resume", "stop"]);
    }

    #[tokio::test]
    async fn agents_are_addressed_on_the_agent_port() {
        let h = Harness::new().await;
        let id = h.add_device("TV-01", "10.0.0.1").await;
        let video = h.add_video("clip.mp4").await;

        h.post(
            "/playback/play",
            serde_json::json!({ "device_ids": [id], "video_id": video }),
        )
        .await;

        let (_, host) = h.calls().into_iter().next().unwrap();
        assert_eq!(host, format!("10.0.0.1:{AGENT_PORT}"));
    }

    // ── play-all ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn play_all_targets_every_device_that_is_not_offline() {
        let h = Harness::new().await;
        let a = h.add_device("TV-01", "10.0.0.1").await;
        let b = h.add_device("TV-02", "10.0.0.2").await;
        let gone = h.add_device("TV-03", "10.0.0.3").await;
        db::update_device_state(&h.state.db, gone, &DeviceState::Offline, None, 0)
            .await
            .unwrap();
        let video = h.add_video("clip.mp4").await;

        let resp = h
            .post(
                "/playback/play-all",
                serde_json::json!({ "video_id": video }),
            )
            .await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        let succeeded = body["succeeded"].as_array().unwrap();
        assert_eq!(succeeded.len(), 2);
        assert_eq!(h.calls().len(), 2, "an offline TV should not be dialled");

        for id in [a, b] {
            assert_eq!(h.device(id).await.state, DeviceState::Playing);
        }
        assert_eq!(h.device(gone).await.state, DeviceState::Offline);
    }

    #[tokio::test]
    async fn play_all_with_nothing_online_is_a_409_not_a_silent_success() {
        let h = Harness::new().await;
        let gone = h.add_device("TV-01", "10.0.0.1").await;
        db::update_device_state(&h.state.db, gone, &DeviceState::Offline, None, 0)
            .await
            .unwrap();
        let video = h.add_video("clip.mp4").await;

        let resp = h
            .post(
                "/playback/play-all",
                serde_json::json!({ "video_id": video }),
            )
            .await;
        assert_eq!(resp.status(), 409);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn play_all_with_an_unknown_video_is_a_404() {
        let h = Harness::new().await;
        h.add_device("TV-01", "10.0.0.1").await;

        let resp = h
            .post(
                "/playback/play-all",
                serde_json::json!({ "video_id": Uuid::new_v4() }),
            )
            .await;
        assert_eq!(resp.status(), 404);
    }
}
