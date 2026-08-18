//! Route table. Handlers live in `handlers/`; this file only wires paths.

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tower_http::services::{ServeDir, ServeFile};

use crate::handlers::{devices, playback, sse, videos};
use crate::state::AppState;

/// The whole application. `main` and the handler tests both build from here, so
/// tests exercise the same routing the server serves.
///
/// `dashboard_dir` is the built React app. Tests pass `None`; without it, a
/// request to `/` is simply a 404 rather than the dashboard.
pub fn app(state: Arc<AppState>, dashboard_dir: Option<&Path>) -> Router {
    // ServeDir rather than a hand-rolled file handler: it already answers Range
    // requests (mpv seeks before the file has finished downloading), sets
    // Content-Type from the extension, handles HEAD and conditional requests,
    // and refuses to escape the directory with `..`.
    let videos = ServeDir::new(&state.videos_dir);

    let mut router = Router::new()
        .nest("/api", api_router())
        .nest_service("/videos", videos);

    if let Some(dir) = dashboard_dir {
        // A single-page app owns its own routing, so an unknown path has to
        // return index.html rather than a 404 — otherwise a deep link or a
        // browser refresh lands on nothing.
        let index = dir.join("index.html");
        router = router.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }

    router.with_state(state)
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
        .route("/playback/play", post(playback::play))
        .route("/playback/play-all", post(playback::play_all))
        .route("/playback/stop", post(playback::stop))
        .route("/playback/pause", post(playback::pause))
        .route("/playback/resume", post(playback::resume))
        .route("/events", get(sse::sse_handler))
        // Without this, an unknown /api path would fall through to the SPA
        // fallback and answer a fetch() with index.html and a 200.
        .fallback(unknown_api_route)
}

async fn unknown_api_route() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "no such API route" })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    struct Harness {
        root: PathBuf,
        base: String,
        client: reqwest::Client,
    }

    /// Spin up the app, optionally with a fake dashboard build present.
    async fn harness(with_dashboard: bool) -> Harness {
        let root =
            std::env::temp_dir().join(format!("tv-controller-router-{}", uuid::Uuid::new_v4()));
        let videos = root.join("videos");
        let dashboard = root.join("dist");
        std::fs::create_dir_all(&videos).unwrap();
        std::fs::create_dir_all(&dashboard).unwrap();
        std::fs::write(dashboard.join("index.html"), b"<!doctype html>SPA ROOT").unwrap();
        std::fs::write(dashboard.join("app.js"), b"console.log(1)").unwrap();

        let pool = db::connect(&format!("sqlite:{}", root.join("test.db").display()))
            .await
            .unwrap();
        let state = AppState::new(pool, "http://host:8000", videos, reqwest::Client::new());

        let router = app(state, with_dashboard.then_some(dashboard.as_path()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Harness {
            root,
            base: format!("http://{addr}"),
            client: reqwest::Client::new(),
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Harness {
        async fn get(&self, path: &str) -> reqwest::Response {
            self.client
                .get(format!("{}{path}", self.base))
                .send()
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn an_unknown_api_route_is_a_json_404() {
        let h = harness(true).await;
        let resp = h.get("/api/nope").await;

        assert_eq!(resp.status(), 404);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["error"].is_string(),
            "expected a JSON error, got {body}"
        );
    }

    /// The trap this fallback exists to avoid: without an /api fallback, an
    /// unknown API path would reach the SPA fallback and answer a fetch() with
    /// index.html and a 200.
    #[tokio::test]
    async fn an_unknown_api_route_is_never_served_the_dashboard() {
        let h = harness(true).await;
        let resp = h.get("/api/devices/typo/extra").await;

        assert_eq!(resp.status(), 404);
        let body = resp.text().await.unwrap();
        assert!(
            !body.contains("SPA ROOT"),
            "an API path fell through to index.html: {body}"
        );
    }

    #[tokio::test]
    async fn the_dashboard_is_served_at_the_root() {
        let h = harness(true).await;
        let resp = h.get("/").await;

        assert_eq!(resp.status(), 200);
        assert!(resp.text().await.unwrap().contains("SPA ROOT"));
    }

    #[tokio::test]
    async fn a_real_asset_is_served_rather_than_the_index() {
        let h = harness(true).await;
        let resp = h.get("/app.js").await;

        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "console.log(1)");
    }

    /// A deep link or a browser refresh must reach the SPA, not a 404.
    #[tokio::test]
    async fn an_unknown_path_falls_back_to_the_single_page_app() {
        let h = harness(true).await;
        let resp = h.get("/some/client/route").await;

        assert_eq!(resp.status(), 200);
        assert!(resp.text().await.unwrap().contains("SPA ROOT"));
    }

    #[tokio::test]
    async fn without_a_dashboard_build_the_root_is_a_404() {
        let h = harness(false).await;
        assert_eq!(h.get("/").await.status(), 404);
        // The API still works.
        assert_eq!(h.get("/api/devices").await.status(), 200);
    }

    #[tokio::test]
    async fn video_files_are_not_shadowed_by_the_dashboard_fallback() {
        let h = harness(true).await;
        // Nothing of that name exists, so this must be a 404 from ServeDir —
        // not the SPA index, which would hand mpv an HTML page.
        let resp = h.get("/videos/missing.mp4").await;

        assert_eq!(resp.status(), 404);
        let body = resp.text().await.unwrap();
        assert!(
            !body.contains("SPA ROOT"),
            "mpv would have got HTML: {body}"
        );
    }
}
