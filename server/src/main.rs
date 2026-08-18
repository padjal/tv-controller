mod db;
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

    Ok(())
}
