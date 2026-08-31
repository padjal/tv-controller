//! Fleet-wide settings.
//!
//! Just one so far: `idle_background`, the library image a TV shows when it has
//! nothing playing. Without it an idle screen shows whatever the Pi's desktop
//! is, which is the Raspberry Pi wallpaper on a fresh install — not what a
//! client wants on a wall during business hours.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use shared::{IdleBackgroundCommand, Settings, SseKind};

use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::services::fan_out;
use crate::state::AppState;

/// `GET /api/settings`
pub async fn get(State(state): State<Arc<AppState>>) -> ApiResult<Settings> {
    Ok(Json(load(&state).await?))
}

/// `PUT /api/settings`
///
/// The filename is validated against the library rather than taken on trust:
/// an unknown name would be pushed to every agent as a URL that 404s, leaving
/// the whole fleet on a black screen with nothing to explain it.
pub async fn put(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Settings>,
) -> ApiResult<Settings> {
    if let Some(filename) = &req.idle_background {
        let video = db::get_video_by_filename(&state.db, filename)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no library file named {filename}")))?;

        if !video.kind.is_image() {
            return Err(ApiError::bad_request(format!(
                "{filename} is a video; the idle background must be an image"
            )));
        }
    }

    db::set_setting(&state.db, db::IDLE_BACKGROUND, req.idle_background.as_deref()).await?;

    let settings = load(&state).await?;
    push_to_devices(&state, &settings).await?;
    state.broadcast(SseKind::SettingsChanged, &settings);

    Ok(Json(settings))
}

async fn load(state: &AppState) -> Result<Settings, ApiError> {
    let idle_background = db::get_setting(&state.db, db::IDLE_BACKGROUND).await?;
    Ok(Settings {
        idle_background_url: idle_background.as_deref().map(|f| state.video_url(f)),
        idle_background,
    })
}

/// Push the new background to every device.
///
/// Failures are logged, not returned: the setting is stored either way, an
/// offline TV picks it up when it next registers, and reporting a 502 because
/// one unplugged screen did not answer would make the dashboard look broken
/// when the change actually took.
async fn push_to_devices(state: &AppState, settings: &Settings) -> Result<(), ApiError> {
    let devices = db::list_devices(&state.db).await?;
    if devices.is_empty() {
        return Ok(());
    }

    let command = IdleBackgroundCommand {
        url: settings.idle_background_url.clone(),
    };

    for (id, result) in fan_out::fan_out_idle_background(&devices, &command, &state.http).await {
        if let Err(err) = result {
            tracing::warn!(%id, "could not set idle background: {err:#}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::api_router;
    use shared::Settings;
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
            let root =
                std::env::temp_dir().join(format!("tv-controller-settings-{}", uuid::Uuid::new_v4()));
            let videos = root.join("videos");
            std::fs::create_dir_all(&videos).unwrap();
            let db = db::connect(&format!("sqlite:{}", root.join("test.db").display()))
                .await
                .unwrap();
            let state = AppState::new(
                db,
                "http://host:8000",
                videos,
                root.join("thumbs"),
                reqwest::Client::new(),
            );

            let app = axum::Router::new()
                .nest("/api", api_router())
                .with_state(state.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });

            Harness {
                root,
                base: format!("http://{addr}/api"),
                state,
                client: reqwest::Client::new(),
            }
        }

        /// Put a row in the library without needing the file to be decodable —
        /// these tests are about the setting, not about scanning.
        async fn add(&self, filename: &str) {
            std::fs::write(self.state.videos_dir.join(filename), b"x").unwrap();
            db::upsert_video(
                &self.state.db,
                filename,
                &self.state.videos_dir.join(filename).to_string_lossy(),
                1,
                None,
            )
            .await
            .unwrap();
        }

        async fn put(&self, body: serde_json::Value) -> reqwest::Response {
            self.client
                .put(format!("{}/settings", self.base))
                .json(&body)
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

    #[tokio::test]
    async fn settings_start_empty_rather_than_erroring() {
        let h = Harness::new().await;
        let resp = h
            .client
            .get(format!("{}/settings", h.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.json::<Settings>().await.unwrap().idle_background, None);
    }

    #[tokio::test]
    async fn setting_an_image_stores_it_and_resolves_a_url() {
        let h = Harness::new().await;
        h.add("logo.png").await;
        let mut rx = h.state.subscribe();

        let settings: Settings = h
            .put(serde_json::json!({ "idle_background": "logo.png" }))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(settings.idle_background.as_deref(), Some("logo.png"));
        assert_eq!(
            settings.idle_background_url.as_deref(),
            Some("http://host:8000/videos/logo.png"),
            "agents need a URL and cannot build one themselves"
        );

        let event = rx.try_recv().expect("SettingsChanged broadcast");
        assert!(matches!(event.kind, SseKind::SettingsChanged));
    }

    /// The filename becomes a URL pushed to every TV in the fleet. An unknown
    /// one would 404 on each of them and leave the whole wall black with
    /// nothing on the dashboard to say why.
    #[tokio::test]
    async fn an_unknown_filename_is_rejected() {
        let h = Harness::new().await;
        let resp = h.put(serde_json::json!({ "idle_background": "nope.png" })).await;
        assert_eq!(resp.status(), 404);
    }

    /// A video would play once and stop, leaving the black screen the setting
    /// exists to avoid.
    #[tokio::test]
    async fn a_video_is_rejected_as_a_background() {
        let h = Harness::new().await;
        h.add("clip.mp4").await;

        let resp = h.put(serde_json::json!({ "idle_background": "clip.mp4" })).await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn the_background_can_be_cleared_again() {
        let h = Harness::new().await;
        h.add("logo.png").await;
        h.put(serde_json::json!({ "idle_background": "logo.png" })).await;

        let settings: Settings = h
            .put(serde_json::json!({ "idle_background": null }))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(settings.idle_background, None);
        assert_eq!(settings.idle_background_url, None);
    }

    #[tokio::test]
    async fn a_name_with_spaces_and_hashes_is_encoded_for_agents() {
        let h = Harness::new().await;
        h.add("winter #2.png").await;

        let settings: Settings = h
            .put(serde_json::json!({ "idle_background": "winter #2.png" }))
            .await
            .json()
            .await
            .unwrap();
        // An unencoded '#' truncates the URL into a fragment and the agent
        // fetches the wrong path entirely.
        assert_eq!(
            settings.idle_background_url.as_deref(),
            Some("http://host:8000/videos/winter%20%232.png")
        );
    }
}
