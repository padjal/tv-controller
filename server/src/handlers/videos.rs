//! Video library metadata.
//!
//! The rows here are maintained by the scanner (`services::video_scan`), not by
//! these handlers — the library is whatever is on disk. Serving the files
//! themselves is a `ServeDir` mounted at `/videos` in `router::app`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use shared::Video;
use uuid::Uuid;

use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// `GET /api/videos`
pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<Vec<Video>> {
    Ok(Json(db::list_videos(&state.db).await?))
}

/// `GET /api/videos/:id`
pub async fn get_one(State(state): State<Arc<AppState>>, Path(id): Path<Uuid>) -> ApiResult<Video> {
    db::get_video(&state.db, id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("no video with id {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::app;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    struct Harness {
        root: PathBuf,
        base: String,
        state: Arc<AppState>,
        client: reqwest::Client,
    }

    impl Harness {
        async fn new() -> Harness {
            let root = std::env::temp_dir().join(format!("tv-controller-vid-{}", Uuid::new_v4()));
            let videos = root.join("videos");
            std::fs::create_dir_all(&videos).unwrap();
            let db = db::connect(&format!("sqlite:{}", root.join("test.db").display()))
                .await
                .unwrap();
            let state = AppState::new(db, "http://host:8000", videos, root.join("thumbs"), reqwest::Client::new());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            let router = app(state.clone(), None);
            tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });

            Harness {
                root,
                base: format!("http://{addr}"),
                state,
                client: reqwest::Client::new(),
            }
        }

        /// Write a file of `bytes` predictable content into the videos dir.
        fn write_video(&self, name: &str, bytes: usize) -> Vec<u8> {
            let content: Vec<u8> = (0..bytes).map(|i| (i % 251) as u8).collect();
            std::fs::write(self.state.videos_dir.join(name), &content).unwrap();
            content
        }

        async fn get(&self, path: &str) -> reqwest::Response {
            self.client
                .get(format!("{}{path}", self.base))
                .send()
                .await
                .unwrap()
        }

        async fn get_range(&self, path: &str, range: &str) -> reqwest::Response {
            self.client
                .get(format!("{}{path}", self.base))
                .header("Range", range)
                .send()
                .await
                .unwrap()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    // ── Metadata endpoints ──────────────────────────────────────────────────

    #[tokio::test]
    async fn list_is_an_empty_array_not_an_error() {
        let h = Harness::new().await;
        let resp = h.get("/api/videos").await;
        assert_eq!(resp.status(), 200);
        let videos: Vec<Video> = resp.json().await.unwrap();
        assert!(videos.is_empty());
    }

    #[tokio::test]
    async fn list_returns_videos_sorted_by_filename() {
        let h = Harness::new().await;
        for name in ["z.mp4", "a.mp4"] {
            db::upsert_video(&h.state.db, name, &format!("/v/{name}"), 10, Some(30))
                .await
                .unwrap();
        }

        let videos: Vec<Video> = h.get("/api/videos").await.json().await.unwrap();
        let names: Vec<_> = videos.iter().map(|v| v.filename.as_str()).collect();
        assert_eq!(names, ["a.mp4", "z.mp4"]);
    }

    #[tokio::test]
    async fn get_one_returns_metadata_or_404() {
        let h = Harness::new().await;
        let video = db::upsert_video(&h.state.db, "clip.mp4", "/v/clip.mp4", 2048, Some(90))
            .await
            .unwrap();

        let found: Video = h
            .get(&format!("/api/videos/{}", video.id))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(found.id, video.id);
        assert_eq!(found.duration_secs, Some(90));
        assert_eq!(found.size_bytes, 2048);

        let missing = h.get(&format!("/api/videos/{}", Uuid::new_v4())).await;
        assert_eq!(missing.status(), 404);
        let body: serde_json::Value = missing.json().await.unwrap();
        assert!(body["error"].is_string(), "404 must be JSON: {body}");
    }

    #[tokio::test]
    async fn a_non_uuid_video_id_is_rejected() {
        let h = Harness::new().await;
        let resp = h.get("/api/videos/not-a-uuid").await;
        assert!(resp.status().is_client_error(), "got {}", resp.status());
    }

    // ── File serving ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_whole_file_is_served_with_a_video_content_type() {
        let h = Harness::new().await;
        let content = h.write_video("clip.mp4", 4096);

        let resp = h.get("/videos/clip.mp4").await;
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("video/mp4"),
            "mpv and browsers both key off Content-Type"
        );
        assert_eq!(resp.bytes().await.unwrap().as_ref(), content.as_slice());
    }

    #[tokio::test]
    async fn the_server_advertises_range_support() {
        let h = Harness::new().await;
        h.write_video("clip.mp4", 4096);

        let resp = h.get("/videos/clip.mp4").await;
        assert_eq!(
            resp.headers()
                .get("accept-ranges")
                .and_then(|v| v.to_str().ok()),
            Some("bytes"),
            "without this a client will not attempt to seek"
        );
    }

    /// The case CLAUDE.md flagged as untested: mpv seeks with a Range request
    /// before the file has finished downloading.
    #[tokio::test]
    async fn a_range_request_returns_exactly_that_slice() {
        let h = Harness::new().await;
        let content = h.write_video("clip.mp4", 4096);

        let resp = h.get_range("/videos/clip.mp4", "bytes=100-199").await;
        assert_eq!(resp.status(), 206, "a range request must be a 206");
        assert_eq!(
            resp.headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok()),
            Some("bytes 100-199/4096")
        );

        let body = resp.bytes().await.unwrap();
        assert_eq!(body.len(), 100);
        assert_eq!(body.as_ref(), &content[100..200]);
    }

    #[tokio::test]
    async fn an_open_ended_range_runs_to_the_end_of_the_file() {
        let h = Harness::new().await;
        let content = h.write_video("clip.mp4", 1000);

        let resp = h.get_range("/videos/clip.mp4", "bytes=900-").await;
        assert_eq!(resp.status(), 206);
        assert_eq!(
            resp.headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok()),
            Some("bytes 900-999/1000")
        );
        assert_eq!(resp.bytes().await.unwrap().as_ref(), &content[900..]);
    }

    #[tokio::test]
    async fn a_suffix_range_returns_the_tail() {
        let h = Harness::new().await;
        let content = h.write_video("clip.mp4", 1000);

        let resp = h.get_range("/videos/clip.mp4", "bytes=-50").await;
        assert_eq!(resp.status(), 206);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), &content[950..]);
    }

    #[tokio::test]
    async fn a_range_past_the_end_of_the_file_is_rejected() {
        let h = Harness::new().await;
        h.write_video("clip.mp4", 100);

        let resp = h.get_range("/videos/clip.mp4", "bytes=5000-6000").await;
        assert_eq!(
            resp.status(),
            416,
            "an unsatisfiable range must not return the whole file"
        );
    }

    #[tokio::test]
    async fn a_filename_with_spaces_is_served() {
        let h = Harness::new().await;
        let content = h.write_video("summer promo #2.mp4", 512);

        // Exactly what AppState::video_url hands to an agent.
        let url = h.state.video_url("summer promo #2.mp4");
        let path = url.strip_prefix("http://host:8000").unwrap();
        assert_eq!(path, "/videos/summer%20promo%20%232.mp4");

        let resp = h.get(path).await;
        assert_eq!(resp.status(), 200, "the URL we hand out must resolve");
        assert_eq!(resp.bytes().await.unwrap().as_ref(), content.as_slice());
    }

    #[tokio::test]
    async fn an_unknown_file_is_a_404() {
        let h = Harness::new().await;
        assert_eq!(h.get("/videos/nope.mp4").await.status(), 404);
    }

    #[tokio::test]
    async fn the_videos_directory_cannot_be_escaped() {
        let h = Harness::new().await;
        // A secret next to the videos dir, not inside it.
        std::fs::write(h.root.join("secret.txt"), b"password").unwrap();

        for attempt in [
            "/videos/../secret.txt",
            "/videos/%2e%2e%2fsecret.txt",
            "/videos/..%2Fsecret.txt",
            "/videos/....//secret.txt",
        ] {
            let resp = h.get(attempt).await;
            let body = resp.text().await.unwrap_or_default();
            assert!(
                !body.contains("password"),
                "{attempt} escaped the videos dir"
            );
        }
    }
}
