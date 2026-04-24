use anyhow::Result;
use shared::RegisterRequest;
use std::sync::Arc;
use tracing::{info, warn};

mod config;
mod mpv;
mod router;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = config::Config::load()?;
    let mpv = Arc::new(mpv::MpvClient::new("/tmp/mpvsocket"));
    mpv.ensure_running().await?;

    let ip = get_local_ip().unwrap_or_else(|| "unknown".to_string());
    let register_body = RegisterRequest {
        id: config.device_id,
        name: config.device_name.clone(),
        ip,
    };
    let register_url = format!("{}/api/devices/register", config.server_url);
    let client = reqwest::Client::new();
    let mut delay = std::time::Duration::from_secs(1);

    loop {
        match client.post(&register_url).json(&register_body).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("registered with server");
                break;
            }
            Ok(resp) => warn!("server returned {}, retrying", resp.status()),
            Err(e) => warn!("failed to reach server: {e}, retrying in {delay:?}"),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_secs(60));
    }

    let addr = format!("0.0.0.0:{}", config.agent_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("listening on {addr}");
    axum::serve(listener, router::build_router(mpv)).await?;
    Ok(())
}

fn get_local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}
