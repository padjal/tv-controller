//! Route table. Handlers live in `handlers/`; this file only wires paths.

use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;

use crate::handlers::devices;
use crate::state::AppState;

/// Everything under `/api`. Mounted by `main`, which adds static file serving
/// around it in Task 3.10.
pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/devices/register", post(devices::register))
        .route("/devices", get(devices::list))
        .route("/devices/:id", get(devices::get_one))
        .route("/devices/:id", delete(devices::delete_one))
}
