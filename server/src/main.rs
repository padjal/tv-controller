mod db;
mod error;
mod handlers;
mod router;
mod services;
mod state;

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:tv-controller.db".to_string());
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8000".to_string())
        .parse()
        .context("PORT must be a number")?;
    // Where `npm run build` puts the dashboard. Relative paths resolve against
    // the working directory, which is /app under docker-compose.
    let dashboard_dir =
        PathBuf::from(std::env::var("DASHBOARD_DIR").unwrap_or_else(|_| "dashboard/dist".into()));

    let pool = db::connect(&database_url).await?;
    let state = AppState::from_env(pool)?;

    let devices = db::list_devices(&state.db).await?;
    tracing::info!(
        base_url = %state.server_base_url,
        videos_dir = %state.videos_dir.display(),
        "database ready ({database_url}): {} device(s) registered",
        devices.len()
    );

    if dashboard_dir.join("index.html").is_file() {
        tracing::info!(dir = %dashboard_dir.display(), "serving dashboard");
    } else {
        // Not fatal — the API is still fully usable — but it is the likely
        // cause of a blank page, so say so once at startup.
        tracing::warn!(
            dir = %dashboard_dir.display(),
            "no dashboard build found (run `npm run build` in dashboard/); / will 404"
        );
    }

    // The watcher scans once before it starts watching, so the library is
    // populated by the time the first request can arrive.
    let scanner = tokio::spawn({
        let state = state.clone();
        async move {
            if let Err(err) = services::video_scan::watch(state).await {
                tracing::error!(%err, "video scanner stopped");
            }
        }
    });

    let heartbeat = tokio::spawn(services::heartbeat::run(
        state.clone(),
        services::heartbeat::build_client()?,
    ));

    let app = router::app(state, Some(&dashboard_dir));

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!("listening on http://{addr}");

    let server = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(%err, "http server stopped");
        }
    });

    // Any of the three exiting means something is wrong; stop rather than limp
    // along with a missing scanner or heartbeat.
    tokio::select! {
        res = scanner => res?,
        res = heartbeat => res?,
        res = server => res?,
        _ = shutdown_signal() => tracing::info!("shutting down"),
    }

    Ok(())
}

/// Ctrl-C, or SIGTERM from systemd/docker on stop.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // Without SIGTERM handling Ctrl-C still works; do not take the
            // process down over it.
            Err(err) => {
                tracing::warn!(%err, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
