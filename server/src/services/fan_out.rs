//! Dispatches one command to many Pi agents at once.
//!
//! Every request fires concurrently and each device's outcome is returned
//! separately, so one unplugged TV never delays or fails the others. Callers
//! use the per-device results to update state only for the agents that
//! actually accepted the command.
//!
//! Note: consumed by the playback handlers (Task 3.8) and the heartbeat (3.5),
//! neither of which exists yet, hence the module-level `dead_code` allow.
//! Remove it once the phase is complete.
#![allow(dead_code)]

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use serde::Serialize;
use shared::{Device, PlayCommand};
use uuid::Uuid;

use super::AGENT_PORT;

/// Default per-request ceiling, applied by [`build_client`]. An agent that has
/// not answered in this long is treated as failed for this command; the
/// heartbeat decides whether it is actually offline.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Agent error bodies are echoed into our logs — keep them bounded.
const MAX_ERROR_BODY: usize = 200;

/// One entry per device, in the order the devices were given.
pub type FanOutResults = Vec<(Uuid, Result<()>)>;

/// The shared HTTP client for talking to agents. Build once and reuse: it
/// pools connections, so a fan-out to twenty TVs does not open twenty fresh
/// sockets every command.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("failed to build HTTP client for agent requests")
}

pub async fn fan_out_play(
    devices: &[Device],
    command: &PlayCommand,
    client: &reqwest::Client,
) -> FanOutResults {
    fan_out_post(devices, "play", command, client).await
}

pub async fn fan_out_stop(devices: &[Device], client: &reqwest::Client) -> FanOutResults {
    fan_out_post(devices, "stop", &empty_body(), client).await
}

pub async fn fan_out_pause(devices: &[Device], client: &reqwest::Client) -> FanOutResults {
    fan_out_post(devices, "pause", &empty_body(), client).await
}

pub async fn fan_out_resume(devices: &[Device], client: &reqwest::Client) -> FanOutResults {
    fan_out_post(devices, "resume", &empty_body(), client).await
}

/// The agent's stop/pause/resume handlers take a body but ignore it.
fn empty_body() -> serde_json::Value {
    serde_json::json!({})
}

async fn fan_out_post<B: Serialize>(
    devices: &[Device],
    endpoint: &str,
    body: &B,
    client: &reqwest::Client,
) -> FanOutResults {
    let requests = devices
        .iter()
        .map(|device| async move { (device.id, post_one(device, endpoint, body, client).await) });

    join_all(requests).await
}

async fn post_one<B: Serialize>(
    device: &Device,
    endpoint: &str,
    body: &B,
    client: &reqwest::Client,
) -> Result<()> {
    let url = format!("{}/{endpoint}", agent_base_url(&device.ip));

    // Requests are bounded by the client's own timeout rather than a per-request
    // one, so a caller that wants a tighter bound (the heartbeat polls far more
    // often than a playback command) can set it. Build clients with
    // `build_client` to get REQUEST_TIMEOUT.
    let response = client.post(&url).json(body).send().await.with_context(|| {
        format!(
            "{} ({}) did not answer POST /{endpoint}",
            device.name, device.ip
        )
    })?;

    // A reachable agent that rejects the command is still a failure — `send`
    // succeeds on a 500, so the status has to be checked explicitly.
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "{} ({}) returned {status} for POST /{endpoint}: {}",
            device.name,
            device.ip,
            truncate(body.trim(), MAX_ERROR_BODY)
        ));
    }

    Ok(())
}

/// `http://host:8080`, bracketing the host if it is an IPv6 literal.
fn agent_base_url(ip: &str) -> String {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V6(addr)) => format!("http://[{addr}]:{AGENT_PORT}"),
        // Hostnames and IPv4 both work unbracketed.
        _ => format!("http://{ip}:{AGENT_PORT}"),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::HOST;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use shared::DeviceState;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Host that the stub agent answers with a 500.
    const FAILING_IP: &str = "10.0.0.99";

    fn device(name: &str, ip: &str) -> Device {
        Device {
            id: Uuid::new_v4(),
            name: name.to_string(),
            ip: ip.to_string(),
            state: DeviceState::Idle,
            current_video: None,
            last_seen: 0,
        }
    }

    /// Spawn a stub agent on an ephemeral port.
    async fn stub_agent(router: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    /// A client that sends every request to the stub regardless of the device
    /// IP in the URL.
    ///
    /// `agent_base_url` always targets AGENT_PORT, and reqwest's `resolve`
    /// override ignores the port, so an HTTP proxy is the way to reach a stub
    /// on an ephemeral port. The stub still sees the intended host in the
    /// `Host` header, which lets it answer per-device.
    fn client_via(addr: SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .proxy(reqwest::Proxy::all(format!("http://{addr}")).unwrap())
            .build()
            .unwrap()
    }

    fn play_command() -> PlayCommand {
        PlayCommand {
            url: "http://server:8000/videos/clip.mp4".to_string(),
            video_id: Uuid::new_v4(),
        }
    }

    #[tokio::test]
    async fn empty_device_list_makes_no_requests() {
        let client = build_client().unwrap();
        let results = fan_out_play(&[], &play_command(), &client).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn play_posts_the_command_to_every_device() {
        let seen = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(std::sync::Mutex::new(Vec::<PlayCommand>::new()));

        let router = {
            let seen = seen.clone();
            let bodies = bodies.clone();
            Router::new().route(
                "/play",
                post(move |Json(cmd): Json<PlayCommand>| {
                    let seen = seen.clone();
                    let bodies = bodies.clone();
                    async move {
                        seen.fetch_add(1, Ordering::SeqCst);
                        bodies.lock().unwrap().push(cmd);
                        Json(serde_json::json!({ "ok": true }))
                    }
                }),
            )
        };
        let addr = stub_agent(router).await;

        let devices = vec![device("TV-01", "10.0.0.1"), device("TV-02", "10.0.0.2")];
        let command = play_command();
        let results = fan_out_play(&devices, &command, &client_via(addr)).await;

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(
            results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            devices.iter().map(|d| d.id).collect::<Vec<_>>(),
            "results must line up with the devices given"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert!(bodies.iter().all(|b| b.url == command.url));
        assert!(bodies.iter().all(|b| b.video_id == command.video_id));
    }

    /// The stub answers FAILING_IP with a 500 and everyone else with 200.
    fn selective_router(endpoint: &str) -> Router {
        Router::new().route(
            &format!("/{endpoint}"),
            post(|headers: HeaderMap| async move {
                let host = headers
                    .get(HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                if host.starts_with(FAILING_IP) {
                    (StatusCode::INTERNAL_SERVER_ERROR, "mpv socket closed").into_response()
                } else {
                    Json(serde_json::json!({ "ok": true })).into_response()
                }
            }),
        )
    }

    #[tokio::test]
    async fn one_failing_device_does_not_affect_the_others() {
        let addr = stub_agent(selective_router("stop")).await;

        let devices = vec![
            device("TV-01", "10.0.0.1"),
            device("TV-BAD", FAILING_IP),
            device("TV-03", "10.0.0.3"),
        ];
        let results = fan_out_stop(&devices, &client_via(addr)).await;

        assert_eq!(results.len(), 3);
        assert_eq!(
            results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            devices.iter().map(|d| d.id).collect::<Vec<_>>()
        );
        assert!(results[0].1.is_ok());
        assert!(
            results[1].1.is_err(),
            "the failing device should report an error"
        );
        assert!(results[2].1.is_ok(), "a later device must still succeed");

        let err = results[1].1.as_ref().unwrap_err().to_string();
        assert!(
            err.contains("TV-BAD"),
            "error should name the device: {err}"
        );
    }

    #[tokio::test]
    async fn an_agent_error_status_is_a_failure() {
        let addr = stub_agent(selective_router("pause")).await;

        let results = fan_out_pause(&[device("TV-BAD", FAILING_IP)], &client_via(addr)).await;

        let err = results[0].1.as_ref().unwrap_err().to_string();
        assert!(err.contains("500"), "should report the status: {err}");
        assert!(
            err.contains("mpv socket closed"),
            "should include the agent's message: {err}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_agent_is_reported_as_failed() {
        // TEST-NET-1: guaranteed not routable, so this exercises the connect
        // failure path rather than a status code. No proxy here.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .unwrap();

        let results = fan_out_resume(&[device("TV-GONE", "192.0.2.1")], &client).await;

        let err = results[0].1.as_ref().unwrap_err().to_string();
        assert!(
            err.contains("TV-GONE"),
            "error should name the device: {err}"
        );
        assert!(
            err.contains("192.0.2.1"),
            "error should name the address: {err}"
        );
    }

    #[tokio::test]
    async fn requests_run_concurrently_not_one_after_another() {
        // Serial dispatch over 5 devices would take 5x as long as concurrent.
        const DELAY: Duration = Duration::from_millis(300);
        let router = Router::new().route(
            "/resume",
            post(|| async {
                tokio::time::sleep(DELAY).await;
                Json(serde_json::json!({ "ok": true }))
            }),
        );
        let addr = stub_agent(router).await;

        let devices: Vec<_> = (0..5)
            .map(|i| device(&format!("TV-0{i}"), &format!("10.0.0.{i}")))
            .collect();
        let started = std::time::Instant::now();
        let results = fan_out_resume(&devices, &client_via(addr)).await;
        let elapsed = started.elapsed();

        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert!(
            elapsed < DELAY * 3,
            "5 requests of {DELAY:?} took {elapsed:?}; they are not running concurrently"
        );
    }

    #[tokio::test]
    async fn a_hanging_agent_times_out_instead_of_blocking_forever() {
        let router = Router::new().route(
            "/stop",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Json(serde_json::json!({ "ok": true }))
            }),
        );
        let addr = stub_agent(router).await;

        // A caller-supplied client with a tighter timeout than REQUEST_TIMEOUT
        // must be honoured, not overridden by a per-request timeout.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .proxy(reqwest::Proxy::all(format!("http://{addr}")).unwrap())
            .build()
            .unwrap();

        let started = std::time::Instant::now();
        let results = fan_out_stop(&[device("TV-01", "10.0.0.1")], &client).await;
        let elapsed = started.elapsed();

        assert!(results[0].1.is_err(), "hanging agent should fail");
        assert!(
            elapsed < Duration::from_secs(2),
            "gave up after {elapsed:?}; the client's 250ms timeout was not honoured"
        );
    }

    #[test]
    fn agent_url_brackets_ipv6_literals() {
        assert_eq!(agent_base_url("192.168.1.11"), "http://192.168.1.11:8080");
        assert_eq!(agent_base_url("tv-01.local"), "http://tv-01.local:8080");
        assert_eq!(agent_base_url("fe80::1"), "http://[fe80::1]:8080");
    }

    #[test]
    fn long_agent_errors_are_truncated() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghijk", 5), "abcde\u{2026}");
        // Must not split a multi-byte character.
        assert_eq!(
            truncate("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}", 2),
            "\u{e9}\u{e9}\u{2026}"
        );
    }
}
