use anyhow::Result;
use shared::{Device, RegisterRequest, Settings};
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
    // Starting mpv is best-effort: the agent has to come up even when mpv does
    // not. A Pi whose display is not ready yet, or whose mpv is missing or
    // wedged, would otherwise exit here — and under `Restart=always` that is a
    // crash loop in which the TV never appears on the dashboard at all, since
    // nothing is left listening to answer /health or /status. The operator
    // sees a device that vanished rather than one reporting a real error.
    //
    // Carrying on costs nothing: `handle_play` calls `ensure_running` before
    // every play, so the first play once mpv is available spawns it, and until
    // then each command fails with the actual reason.
    if let Err(e) = mpv.ensure_running().await {
        warn!("mpv unavailable at startup: {e:#} — serving anyway, the next play will retry");
    }

    let ip = get_local_ip().unwrap_or_else(|| "unknown".to_string());
    let register_body = RegisterRequest {
        id: config.device_id,
        name: config.device_name.clone(),
        ip,
    };
    let register_url = format!("{}/api/devices/register", config.server_url);
    let client = reqwest::Client::new();

    let settings_url = format!("{}/api/settings", config.server_url);
    let startup_mpv = Arc::clone(&mpv);

    // Registration runs in the background so the HTTP server is available immediately.
    tokio::spawn(async move {
        let mut delay = std::time::Duration::from_secs(1);
        loop {
            match client.post(&register_url).json(&register_body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!("registered with server");
                    // The server is the authority on both of these and the
                    // agent holds them only in memory, so they have to be
                    // re-learned on every start. Without it a rebooted Pi
                    // comes back unrotated, showing a black screen, and stays
                    // that way until someone touches the dashboard.
                    restore_device_settings(&startup_mpv, resp, &client, &settings_url).await;
                    break;
                }
                Ok(resp) => warn!("server returned {}, retrying", resp.status()),
                Err(e) => warn!("failed to reach server: {e}, retrying in {delay:?}"),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(std::time::Duration::from_secs(60));
        }
    });

    let addr = format!("0.0.0.0:{}", config.agent_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("listening on {addr}");
    axum::serve(listener, router::build_router(mpv)).await?;
    Ok(())
}

/// Re-apply the rotation and idle background the server holds for this device.
///
/// Best-effort throughout: a failure here must not stop the agent serving.
/// Rotation comes from the register response, which is the device row itself;
/// the idle background is fleet-wide and needs its own request.
async fn restore_device_settings(
    mpv: &Arc<mpv::MpvClient>,
    resp: reqwest::Response,
    client: &reqwest::Client,
    settings_url: &str,
) {
    match resp.json::<Device>().await {
        Ok(device) if device.rotation != 0 => {
            if let Err(e) = mpv.set_rotation(device.rotation).await {
                warn!("could not restore rotation {}: {e:#}", device.rotation);
            } else {
                info!("restored rotation {}", device.rotation);
            }
        }
        Ok(_) => {}
        Err(e) => warn!("could not read register response: {e}"),
    }

    match client.get(settings_url).send().await {
        Ok(resp) => match resp.json::<Settings>().await {
            Ok(settings) => {
                if let Some(url) = settings.idle_background_url {
                    if let Err(e) = mpv.set_idle_background(Some(url)).await {
                        warn!("could not show idle background: {e:#}");
                    } else {
                        info!("restored idle background");
                    }
                }
            }
            Err(e) => warn!("could not read settings: {e}"),
        },
        Err(e) => warn!("could not fetch settings: {e}"),
    }
}

fn get_local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}
