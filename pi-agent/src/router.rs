use crate::mpv::MpvClient;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use shared::PlayCommand;
use std::sync::Arc;

type AppResult<T> = Result<Json<T>, AppError>;

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

#[derive(Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    hostname: String,
    ip: String,
}

pub fn build_router(mpv: Arc<MpvClient>) -> Router {
    Router::new()
        .route("/play", post(handle_play))
        .route("/stop", post(handle_stop))
        .route("/pause", post(handle_pause))
        .route("/resume", post(handle_resume))
        .route("/status", get(handle_status))
        .route("/health", get(handle_health))
        .with_state(mpv)
}

async fn handle_play(
    State(mpv): State<Arc<MpvClient>>,
    Json(cmd): Json<PlayCommand>,
) -> AppResult<OkResponse> {
    // main spawns mpv at startup, but nothing supervises it after that —
    // systemd watches this process, not its child. Re-checking here is what
    // makes a crashed mpv recoverable: the next play respawns it instead of
    // the TV staying dead until someone restarts the unit.
    //
    // A respawn waits up to 5 s for the IPC socket, which is the server's own
    // per-command ceiling, so the play that triggers a respawn may be reported
    // as failed even though mpv came up. The retry then succeeds.
    mpv.ensure_running().await?;
    mpv.play(&cmd.url, cmd.video_id).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn handle_stop(State(mpv): State<Arc<MpvClient>>) -> AppResult<OkResponse> {
    mpv.stop().await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn handle_pause(State(mpv): State<Arc<MpvClient>>) -> AppResult<OkResponse> {
    mpv.pause().await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn handle_resume(State(mpv): State<Arc<MpvClient>>) -> AppResult<OkResponse> {
    mpv.resume().await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn handle_status(State(mpv): State<Arc<MpvClient>>) -> AppResult<shared::AgentStatus> {
    Ok(Json(mpv.get_status().await?))
}

async fn handle_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        hostname: get_hostname(),
        ip: get_local_ip().unwrap_or_else(|| "unknown".to_string()),
    })
}

fn get_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}
