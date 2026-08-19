//! Device registration and management.
//!
//! Agents call `/register` on boot; the dashboard uses the rest.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use shared::{Device, RegisterRequest, SseKind};
use uuid::Uuid;

use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize)]
pub struct DeleteResponse {
    pub id: Uuid,
    pub deleted: bool,
}

/// `POST /api/devices/register` — an agent announcing itself at boot.
///
/// Idempotent: an agent that reboots, or is restarted repeatedly, re-registers
/// with the same id and only refreshes its name/ip.
pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Device> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("device name must not be empty"));
    }
    if req.ip.trim().is_empty() {
        return Err(ApiError::bad_request("device ip must not be empty"));
    }

    let device = db::upsert_device(&state.db, &req).await?;
    tracing::info!(name = %device.name, ip = %device.ip, id = %device.id, "device registered");

    state.broadcast(SseKind::DeviceUpdated, &device);

    Ok(Json(device))
}

/// `GET /api/devices`
pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<Vec<Device>> {
    Ok(Json(db::list_devices(&state.db).await?))
}

/// `GET /api/devices/:id`
pub async fn get_one(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Device> {
    db::get_device(&state.db, id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("no device with id {id}")))
}

/// `DELETE /api/devices/:id`
///
/// Note: no SSE event is published — `SseKind` has no "removed" variant, so
/// other open dashboards keep showing the tile until they refresh.
pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> ApiResult<DeleteResponse> {
    if !db::delete_device(&state.db, id).await? {
        return Err(ApiError::not_found(format!("no device with id {id}")));
    }

    tracing::info!(%id, "device removed");
    Ok(Json(DeleteResponse { id, deleted: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::api_router;
    use crate::state::AppState;
    use shared::DeviceState;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    /// A live server on an ephemeral port, exercised over real HTTP so that
    /// routing, extraction and serialization are all covered.
    struct Harness {
        root: PathBuf,
        base: String,
        state: Arc<AppState>,
        client: reqwest::Client,
    }

    impl Harness {
        async fn new() -> Harness {
            let root = std::env::temp_dir().join(format!("tv-controller-api-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let db = db::connect(&format!("sqlite:{}", root.join("test.db").display()))
                .await
                .unwrap();
            let state = AppState::new(
                db,
                "http://host:8000",
                root.join("videos"),
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

        async fn register(&self, name: &str, ip: &str) -> reqwest::Response {
            self.client
                .post(format!("{}/devices/register", self.base))
                .json(&serde_json::json!({
                    "id": Uuid::new_v4(),
                    "name": name,
                    "ip": ip,
                }))
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
    async fn register_creates_a_device_and_announces_it() {
        let h = Harness::new().await;
        let mut rx = h.state.subscribe();

        let resp = h.register("TV-01", "192.168.1.11").await;
        assert_eq!(resp.status(), 200);

        let device: Device = resp.json().await.unwrap();
        assert_eq!(device.name, "TV-01");
        assert_eq!(device.ip, "192.168.1.11");
        assert_eq!(device.state, DeviceState::Idle);

        let event = rx.try_recv().expect("DeviceUpdated broadcast");
        assert!(matches!(event.kind, SseKind::DeviceUpdated));
        assert_eq!(event.payload["name"], "TV-01");
    }

    #[tokio::test]
    async fn re_registering_the_same_id_is_idempotent() {
        let h = Harness::new().await;
        let id = Uuid::new_v4();
        let body = |name: &str, ip: &str| serde_json::json!({ "id": id, "name": name, "ip": ip });

        for (name, ip) in [("TV-01", "192.168.1.11"), ("TV-01b", "192.168.1.12")] {
            let resp = h
                .client
                .post(format!("{}/devices/register", h.base))
                .json(&body(name, ip))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }

        let devices: Vec<Device> = h
            .client
            .get(format!("{}/devices", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(devices.len(), 1, "a reboot must not create a second row");
        assert_eq!(devices[0].name, "TV-01b");
        assert_eq!(devices[0].ip, "192.168.1.12");
    }

    #[tokio::test]
    async fn register_rejects_a_blank_name_or_ip() {
        let h = Harness::new().await;

        for (name, ip) in [("   ", "192.168.1.11"), ("TV-01", "")] {
            let resp = h.register(name, ip).await;
            assert_eq!(resp.status(), 400, "name={name:?} ip={ip:?}");
            let body: serde_json::Value = resp.json().await.unwrap();
            assert!(body["error"].is_string(), "errors must be JSON, got {body}");
        }
    }

    #[tokio::test]
    async fn register_rejects_a_malformed_body() {
        let h = Harness::new().await;
        let resp = h
            .client
            .post(format!("{}/devices/register", h.base))
            .json(&serde_json::json!({ "name": "TV-01" })) // no id, no ip
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "expected 4xx, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn list_returns_devices_sorted_by_name() {
        let h = Harness::new().await;
        h.register("TV-02", "192.168.1.12").await;
        h.register("TV-01", "192.168.1.11").await;

        let devices: Vec<Device> = h
            .client
            .get(format!("{}/devices", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["TV-01", "TV-02"]);
    }

    #[tokio::test]
    async fn list_is_an_empty_array_not_an_error() {
        let h = Harness::new().await;
        let resp = h
            .client
            .get(format!("{}/devices", h.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let devices: Vec<Device> = resp.json().await.unwrap();
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn get_one_returns_the_device_or_404() {
        let h = Harness::new().await;
        let created: Device = h
            .register("TV-01", "192.168.1.11")
            .await
            .json()
            .await
            .unwrap();

        let found: Device = h
            .client
            .get(format!("{}/devices/{}", h.base, created.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(found.id, created.id);

        let missing = h
            .client
            .get(format!("{}/devices/{}", h.base, Uuid::new_v4()))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);
        let body: serde_json::Value = missing.json().await.unwrap();
        assert!(body["error"].is_string(), "404 must still be JSON: {body}");
    }

    #[tokio::test]
    async fn a_non_uuid_id_is_rejected_rather_than_treated_as_missing() {
        let h = Harness::new().await;
        let resp = h
            .client
            .get(format!("{}/devices/not-a-uuid", h.base))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "expected 4xx, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn delete_removes_the_device_and_is_not_repeatable() {
        let h = Harness::new().await;
        let created: Device = h
            .register("TV-01", "192.168.1.11")
            .await
            .json()
            .await
            .unwrap();

        let resp = h
            .client
            .delete(format!("{}/devices/{}", h.base, created.id))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: DeleteResponseBody = resp.json().await.unwrap();
        assert!(body.deleted);
        assert_eq!(body.id, created.id);

        // Gone from the list, and a second delete is a 404.
        let devices: Vec<Device> = h
            .client
            .get(format!("{}/devices", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(devices.is_empty());

        let again = h
            .client
            .delete(format!("{}/devices/{}", h.base, created.id))
            .send()
            .await
            .unwrap();
        assert_eq!(again.status(), 404);
    }

    /// Mirrors `DeleteResponse`, which is serialize-only.
    #[derive(serde::Deserialize)]
    struct DeleteResponseBody {
        id: Uuid,
        deleted: bool,
    }
}
