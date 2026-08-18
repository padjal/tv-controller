//! Route table. Handlers live in `handlers/`; this file only wires paths.

use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::services::ServeDir;

use crate::handlers::{devices, videos};
use crate::state::AppState;

/// The whole application. `main` and the handler tests both build from here, so
/// tests exercise the same routing the server serves.
pub fn app(state: Arc<AppState>) -> Router {
    // ServeDir rather than a hand-rolled file handler: it already answers Range
    // requests (mpv seeks before the file has finished downloading), sets
    // Content-Type from the extension, handles HEAD and conditional requests,
    // and refuses to escape the directory with `..`.
    let videos_dir = ServeDir::new(&state.videos_dir);

    Router::new()
        .nest("/api", api_router())
        .nest_service("/videos", videos_dir)
        // The dashboard's static files land here in Task 3.10.
        .with_state(state)
}

/// Everything under `/api`.
pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/devices/register", post(devices::register))
        .route("/devices", get(devices::list))
        .route("/devices/:id", get(devices::get_one))
        .route("/devices/:id", delete(devices::delete_one))
        .route("/videos", get(videos::list))
        .route("/videos/:id", get(videos::get_one))
}
