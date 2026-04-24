use anyhow::{anyhow, Result};
use shared::{AgentStatus, DeviceState};
use std::path::PathBuf;

// AtomicU64 counter used only on Unix where the IPC socket is available.
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub struct MpvClient {
    #[cfg_attr(not(unix), allow(dead_code))]
    socket_path: PathBuf,
}

impl MpvClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self { socket_path: socket_path.into() }
    }

    /// Checks whether mpv is already responsive; spawns it if not.
    pub async fn ensure_running(&self) -> Result<()> {
        if self.ping().await.is_ok() {
            return Ok(());
        }
        tokio::process::Command::new("mpv")
            .args(["--input-ipc-server=/tmp/mpvsocket", "--idle=yes", "--no-terminal"])
            .spawn()
            .map_err(|e| anyhow!("failed to spawn mpv: {e}"))?;
        // Poll until the IPC socket is ready, up to 5 s.
        for _ in 0..20 {
            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
            if self.ping().await.is_ok() {
                return Ok(());
            }
        }
        Err(anyhow!("mpv did not become ready within 5 seconds"))
    }

    async fn ping(&self) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["get_version"]})).await?;
        Ok(())
    }

    /// Sends a JSON command to mpv over its IPC socket and returns the `data` field
    /// of the response.
    ///
    /// Each command is tagged with a unique `request_id`. mpv also emits unsolicited
    /// event lines on the same connection; those are skipped until the line whose
    /// `request_id` matches the one we sent arrives.  This prevents mistaking an
    /// event notification for a command reply.
    pub async fn send_command(&self, cmd: serde_json::Value) -> Result<serde_json::Value> {
        #[cfg(unix)]
        {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            use tokio::net::UnixStream;

            let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
            let mut cmd = cmd;
            cmd["request_id"] = serde_json::json!(request_id);

            let stream = UnixStream::connect(&self.socket_path)
                .await
                .map_err(|e| anyhow!("connect to mpv socket: {e}"))?;
            let (read_half, mut write_half) = tokio::io::split(stream);

            let payload = format!("{}\n", serde_json::to_string(&cmd)?);
            write_half.write_all(payload.as_bytes()).await?;

            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await? {
                let val: serde_json::Value = serde_json::from_str(&line)?;
                if val.get("request_id").and_then(|v| v.as_u64()) == Some(request_id) {
                    let err_str = val
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    if err_str != "success" {
                        return Err(anyhow!("mpv error: {err_str}"));
                    }
                    return Ok(val["data"].clone());
                }
                // Line did not match our request_id — it was an event; continue reading.
            }
            Err(anyhow!("mpv socket closed before responding"))
        }

        #[cfg(not(unix))]
        {
            let _ = cmd;
            Err(anyhow!("mpv IPC is only supported on Unix"))
        }
    }

    async fn get_property(&self, name: &str) -> Result<serde_json::Value> {
        self.send_command(serde_json::json!({"command": ["get_property", name]}))
            .await
    }

    pub async fn play(&self, url: &str) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["loadfile", url, "replace"]}))
            .await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["stop"]})).await?;
        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["set_property", "pause", true]}))
            .await?;
        Ok(())
    }

    pub async fn resume(&self) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["set_property", "pause", false]}))
            .await?;
        Ok(())
    }

    pub async fn get_status(&self) -> Result<AgentStatus> {
        let idle = self
            .get_property("idle-active")
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let paused = self
            .get_property("pause")
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let position_secs = self
            .get_property("time-pos")
            .await
            .ok()
            .and_then(|v| v.as_f64());
        let duration_secs = self
            .get_property("duration")
            .await
            .ok()
            .and_then(|v| v.as_f64());

        let state = if idle {
            DeviceState::Idle
        } else if paused {
            DeviceState::Paused
        } else {
            DeviceState::Playing
        };

        Ok(AgentStatus {
            state,
            current_video_id: None,
            position_secs,
            duration_secs,
        })
    }
}
