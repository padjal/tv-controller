mod db;
mod error;
mod handlers;
mod router;
mod services;
mod state;

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

    let pool = db::connect(&database_url).await?;
    let state = AppState::from_env(pool)?;

    let devices = db::list_devices(&state.db).await?;
    tracing::info!(
        base_url = %state.server_base_url,
        videos_dir = %state.videos_dir.display(),
        "database ready ({database_url}): {} device(s) registered",
        devices.len()
    );

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

    // Static file serving (dashboard + /videos) arrives with Task 3.10.
    let app = axum::Router::new()
        .nest("/api", router::api_router())
        .with_state(state);

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
    }

    Ok(())
}
