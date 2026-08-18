//! All database access lives here. Handlers and services call these functions —
//! no inline SQL anywhere else (see CLAUDE.md conventions).
//!
//! Note: some functions are consumed by later Phase 3 tasks (devices/videos/
//! playback handlers, video scan, heartbeat) and are not wired in yet, hence the
//! module-level `dead_code` allow. Remove it once the phase is complete.
#![allow(dead_code)]

use anyhow::{Context, Result};
use shared::{Device, DeviceState, RegisterRequest, Video};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Shared handle to the SQLite connection pool.
pub type Db = SqlitePool;

/// Open (creating if needed) the SQLite database at `database_url` and run all
/// pending migrations. `database_url` is an sqlx connection string, e.g.
/// `sqlite:tv-controller.db`.
pub async fn connect(database_url: &str) -> Result<Db> {
    let opts = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("invalid database url: {database_url}"))?
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
        .context("failed to open sqlite database")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to run database migrations")?;

    Ok(pool)
}

// ── Devices ────────────────────────────────────────────────────────────────────

/// Insert a newly-registered device, or update its name/ip/last_seen if it
/// already exists. Registration never clobbers live playback state, so `state`
/// and `current_video` are left untouched on conflict.
pub async fn upsert_device(db: &Db, req: &RegisterRequest) -> Result<Device> {
    let now = current_unix();
    sqlx::query(
        "INSERT INTO devices (id, name, ip, state, current_video, last_seen)
         VALUES (?1, ?2, ?3, 'Idle', NULL, ?4)
         ON CONFLICT(id) DO UPDATE SET
             name = ?2,
             ip = ?3,
             last_seen = ?4",
    )
    .bind(req.id.to_string())
    .bind(&req.name)
    .bind(&req.ip)
    .bind(now)
    .execute(db)
    .await
    .context("failed to upsert device")?;

    get_device(db, req.id)
        .await?
        .context("device missing immediately after upsert")
}

/// List all known devices, ordered by name for a stable dashboard layout.
pub async fn list_devices(db: &Db) -> Result<Vec<Device>> {
    let rows = sqlx::query(
        "SELECT id, name, ip, state, current_video, last_seen
         FROM devices
         ORDER BY name",
    )
    .fetch_all(db)
    .await
    .context("failed to list devices")?;

    rows.into_iter().map(row_to_device).collect()
}

/// Fetch a single device by id, or `None` if it is not registered.
pub async fn get_device(db: &Db, id: Uuid) -> Result<Option<Device>> {
    let row = sqlx::query(
        "SELECT id, name, ip, state, current_video, last_seen
         FROM devices
         WHERE id = ?1",
    )
    .bind(id.to_string())
    .fetch_optional(db)
    .await
    .context("failed to fetch device")?;

    row.map(row_to_device).transpose()
}

/// Update a device's playback state, current video, and last-seen timestamp.
/// Used by the playback handlers (on command) and the heartbeat task (on poll).
pub async fn update_device_state(
    db: &Db,
    id: Uuid,
    state: &DeviceState,
    current_video: Option<&str>,
    last_seen: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE devices
         SET state = ?2, current_video = ?3, last_seen = ?4
         WHERE id = ?1",
    )
    .bind(id.to_string())
    .bind(state_to_str(state))
    .bind(current_video)
    .bind(last_seen)
    .execute(db)
    .await
    .context("failed to update device state")?;

    Ok(())
}

/// Remove a device. Returns `true` if a row was actually deleted.
pub async fn delete_device(db: &Db, id: Uuid) -> Result<bool> {
    let res = sqlx::query("DELETE FROM devices WHERE id = ?1")
        .bind(id.to_string())
        .execute(db)
        .await
        .context("failed to delete device")?;

    Ok(res.rows_affected() > 0)
}

// ── Videos ─────────────────────────────────────────────────────────────────────

/// Insert or update a video row keyed by filename. Called by the video scanner;
/// `path` and metadata are refreshed on conflict so a re-encoded file updates in
/// place while keeping its stable id.
pub async fn upsert_video(
    db: &Db,
    filename: &str,
    path: &str,
    size_bytes: u64,
    duration_secs: Option<u32>,
) -> Result<Video> {
    let id = Uuid::new_v4().to_string();
    let now = current_unix();
    sqlx::query(
        "INSERT INTO videos (id, filename, path, size_bytes, duration_secs, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(filename) DO UPDATE SET
             path = ?3,
             size_bytes = ?4,
             duration_secs = ?5",
    )
    .bind(&id)
    .bind(filename)
    .bind(path)
    .bind(size_bytes as i64)
    .bind(duration_secs.map(|d| d as i64))
    .bind(now)
    .execute(db)
    .await
    .context("failed to upsert video")?;

    get_video_by_filename(db, filename)
        .await?
        .context("video missing immediately after upsert")
}

/// List all videos, ordered by filename.
pub async fn list_videos(db: &Db) -> Result<Vec<Video>> {
    let rows = sqlx::query(
        "SELECT id, filename, duration_secs, size_bytes
         FROM videos
         ORDER BY filename",
    )
    .fetch_all(db)
    .await
    .context("failed to list videos")?;

    rows.into_iter().map(row_to_video).collect()
}

/// Fetch a single video's metadata by id.
pub async fn get_video(db: &Db, id: Uuid) -> Result<Option<Video>> {
    let row = sqlx::query(
        "SELECT id, filename, duration_secs, size_bytes
         FROM videos
         WHERE id = ?1",
    )
    .bind(id.to_string())
    .fetch_optional(db)
    .await
    .context("failed to fetch video")?;

    row.map(row_to_video).transpose()
}

/// Fetch a video by its (unique) filename.
pub async fn get_video_by_filename(db: &Db, filename: &str) -> Result<Option<Video>> {
    let row = sqlx::query(
        "SELECT id, filename, duration_secs, size_bytes
         FROM videos
         WHERE filename = ?1",
    )
    .bind(filename)
    .fetch_optional(db)
    .await
    .context("failed to fetch video by filename")?;

    row.map(row_to_video).transpose()
}

/// Return the on-disk size the scanner previously recorded for `filename`, if
/// any. Lets the scanner skip re-probing files that have not changed.
pub async fn video_size(db: &Db, filename: &str) -> Result<Option<u64>> {
    let row = sqlx::query("SELECT size_bytes FROM videos WHERE filename = ?1")
        .bind(filename)
        .fetch_optional(db)
        .await
        .context("failed to fetch video size")?;

    Ok(row.map(|r| r.get::<i64, _>("size_bytes") as u64))
}

// ── Row mapping ─────────────────────────────────────────────────────────────────

fn row_to_device(row: SqliteRow) -> Result<Device> {
    let id: String = row.try_get("id")?;
    let state: String = row.try_get("state")?;
    Ok(Device {
        id: Uuid::parse_str(&id).context("invalid device id in database")?,
        name: row.try_get("name")?,
        ip: row.try_get("ip")?,
        state: state_from_str(&state),
        current_video: row.try_get("current_video")?,
        last_seen: row.try_get("last_seen")?,
    })
}

fn row_to_video(row: SqliteRow) -> Result<Video> {
    let id: String = row.try_get("id")?;
    let size_bytes: i64 = row.try_get("size_bytes")?;
    let duration_secs: Option<i64> = row.try_get("duration_secs")?;
    Ok(Video {
        id: Uuid::parse_str(&id).context("invalid video id in database")?,
        filename: row.try_get("filename")?,
        duration_secs: duration_secs.map(|d| d as u32),
        size_bytes: size_bytes as u64,
    })
}

// ── DeviceState <-> TEXT ─────────────────────────────────────────────────────────

fn state_to_str(state: &DeviceState) -> &'static str {
    match state {
        DeviceState::Idle => "Idle",
        DeviceState::Playing => "Playing",
        DeviceState::Paused => "Paused",
        DeviceState::Offline => "Offline",
    }
}

fn state_from_str(s: &str) -> DeviceState {
    match s {
        "Playing" => DeviceState::Playing,
        "Paused" => DeviceState::Paused,
        "Offline" => DeviceState::Offline,
        // Unknown/legacy values fall back to Idle rather than failing a read.
        _ => DeviceState::Idle,
    }
}

fn current_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway on-disk database, removed on drop.
    ///
    /// Deliberately not `sqlite::memory:` — every pooled connection would get its
    /// own private in-memory database, so writes and reads could land on
    /// different ones.
    struct TempDb {
        path: std::path::PathBuf,
        pool: Db,
    }

    impl TempDb {
        async fn new() -> TempDb {
            let path =
                std::env::temp_dir().join(format!("tv-controller-test-{}.db", Uuid::new_v4()));
            let pool = connect(&format!("sqlite:{}", path.display()))
                .await
                .expect("connect + migrate");
            TempDb { path, pool }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn register(name: &str, ip: &str) -> RegisterRequest {
        RegisterRequest {
            id: Uuid::new_v4(),
            name: name.to_string(),
            ip: ip.to_string(),
        }
    }

    #[tokio::test]
    async fn migrations_leave_empty_tables() {
        let t = TempDb::new().await;
        assert!(list_devices(&t.pool).await.unwrap().is_empty());
        assert!(list_videos(&t.pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn device_registers_and_reads_back() {
        let t = TempDb::new().await;
        let req = register("TV-01", "192.168.1.11");

        let device = upsert_device(&t.pool, &req).await.unwrap();
        assert_eq!(device.id, req.id);
        assert_eq!(device.name, "TV-01");
        assert_eq!(device.state, DeviceState::Idle);
        assert_eq!(device.current_video, None);

        let fetched = get_device(&t.pool, req.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, req.id);
        assert!(get_device(&t.pool, Uuid::new_v4()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn re_registering_updates_address_but_not_playback_state() {
        let t = TempDb::new().await;
        let req = register("TV-01", "192.168.1.11");
        upsert_device(&t.pool, &req).await.unwrap();
        update_device_state(
            &t.pool,
            req.id,
            &DeviceState::Playing,
            Some("clip.mp4"),
            1000,
        )
        .await
        .unwrap();

        let renamed = RegisterRequest {
            id: req.id,
            name: "TV-01-renamed".to_string(),
            ip: "192.168.1.12".to_string(),
        };
        let device = upsert_device(&t.pool, &renamed).await.unwrap();

        assert_eq!(device.name, "TV-01-renamed");
        assert_eq!(device.ip, "192.168.1.12");
        assert_eq!(device.state, DeviceState::Playing);
        assert_eq!(device.current_video.as_deref(), Some("clip.mp4"));
        assert!(device.last_seen > 1000, "last_seen should be refreshed");
        assert_eq!(list_devices(&t.pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_device_state_survives_a_round_trip() {
        let t = TempDb::new().await;
        let req = register("TV-01", "192.168.1.11");
        upsert_device(&t.pool, &req).await.unwrap();

        for state in [
            DeviceState::Playing,
            DeviceState::Paused,
            DeviceState::Offline,
            DeviceState::Idle,
        ] {
            update_device_state(&t.pool, req.id, &state, None, 42)
                .await
                .unwrap();
            let device = get_device(&t.pool, req.id).await.unwrap().unwrap();
            assert_eq!(device.state, state);
            assert_eq!(device.last_seen, 42);
        }
    }

    #[tokio::test]
    async fn devices_list_sorted_and_deletable() {
        let t = TempDb::new().await;
        let b = register("TV-02", "192.168.1.12");
        let a = register("TV-01", "192.168.1.11");
        upsert_device(&t.pool, &b).await.unwrap();
        upsert_device(&t.pool, &a).await.unwrap();

        let names: Vec<_> = list_devices(&t.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["TV-01", "TV-02"]);

        assert!(delete_device(&t.pool, a.id).await.unwrap());
        assert!(!delete_device(&t.pool, a.id).await.unwrap());
        assert_eq!(list_devices(&t.pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rescanning_a_video_updates_metadata_and_keeps_its_id() {
        let t = TempDb::new().await;
        let first = upsert_video(&t.pool, "clip.mp4", "/videos/clip.mp4", 1_024, Some(30))
            .await
            .unwrap();
        assert_eq!(first.size_bytes, 1_024);
        assert_eq!(first.duration_secs, Some(30));

        let second = upsert_video(&t.pool, "clip.mp4", "/media/clip.mp4", 2_048, Some(45))
            .await
            .unwrap();
        assert_eq!(second.id, first.id, "id must stay stable across rescans");
        assert_eq!(second.size_bytes, 2_048);
        assert_eq!(second.duration_secs, Some(45));

        assert_eq!(list_videos(&t.pool).await.unwrap().len(), 1);
        assert_eq!(video_size(&t.pool, "clip.mp4").await.unwrap(), Some(2_048));
        assert_eq!(video_size(&t.pool, "missing.mp4").await.unwrap(), None);
    }

    #[tokio::test]
    async fn video_lookups_by_id_and_filename_agree() {
        let t = TempDb::new().await;
        let video = upsert_video(&t.pool, "clip.mkv", "/videos/clip.mkv", 99, None)
            .await
            .unwrap();
        assert_eq!(video.duration_secs, None);

        let by_id = get_video(&t.pool, video.id).await.unwrap().unwrap();
        let by_name = get_video_by_filename(&t.pool, "clip.mkv")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_id.id, video.id);
        assert_eq!(by_name.id, video.id);

        assert!(get_video(&t.pool, Uuid::new_v4()).await.unwrap().is_none());
        assert!(get_video_by_filename(&t.pool, "nope.mp4")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn large_video_size_is_not_truncated() {
        let t = TempDb::new().await;
        // Above u32::MAX — catches a narrowing cast in the i64 <-> u64 mapping.
        let big = 8_000_000_000u64;
        let video = upsert_video(&t.pool, "big.mp4", "/videos/big.mp4", big, Some(7_200))
            .await
            .unwrap();
        assert_eq!(video.size_bytes, big);
        assert_eq!(video_size(&t.pool, "big.mp4").await.unwrap(), Some(big));
    }
}
