mod db;
mod services;
mod state;

use anyhow::Result;

use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:tv-controller.db".to_string());

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

    // Until the router lands in Task 3.10, the background tasks are the whole
    // process; either one exiting means something is wrong, so stop.
    tokio::select! {
        res = scanner => res?,
        res = heartbeat => res?,
    }

    Ok(())
}
