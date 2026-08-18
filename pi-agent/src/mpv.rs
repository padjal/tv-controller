use anyhow::{anyhow, Result};
use shared::{AgentStatus, DeviceState};
use std::path::PathBuf;
use std::time::Duration;

// AtomicU64 counter used only on Unix where the IPC socket is available.
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Ceiling on a single IPC round trip.
///
/// A healthy mpv answers `loadfile` or `get_property` in microseconds, so this
/// is not a latency budget — it is the bound that stops a *wedged* mpv (one
/// blocked on a stalled HTTP read, or stuck in a decode it cannot finish) from
/// hanging the axum handler that called it, and with it the agent's reply to
/// the server. Without it the server sees no answer at all rather than an
/// error, and the dashboard reports the TV offline with no explanation.
///
/// [`MpvClient::get_status`] issues four commands in sequence, so a completely
/// unresponsive mpv can stretch `GET /status` to four times this. That is
/// deliberate: the server's own 3 s status timeout fires first and the device
/// is reported unhealthy, which is the right answer once mpv has stopped
/// talking.
const IPC_TIMEOUT: Duration = Duration::from_secs(2);

pub struct MpvClient {
    #[cfg_attr(not(unix), allow(dead_code))]
    socket_path: PathBuf,
    #[cfg_attr(not(unix), allow(dead_code))]
    ipc_timeout: Duration,
}

impl MpvClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self { socket_path: socket_path.into(), ipc_timeout: IPC_TIMEOUT }
    }

    /// The same client with a shorter fuse, so the timeout test does not have
    /// to sleep out a whole [`IPC_TIMEOUT`].
    #[cfg(test)]
    fn with_ipc_timeout(socket_path: impl Into<PathBuf>, ipc_timeout: Duration) -> Self {
        Self { socket_path: socket_path.into(), ipc_timeout }
    }

    /// Checks whether mpv is already responsive; spawns it if not.
    ///
    /// Called before every playback command, not only at startup: mpv is a
    /// child process nothing supervises — systemd watches the agent, not its
    /// child — so without this a single mpv crash leaves the TV dead until
    /// someone restarts the unit by hand. When mpv is healthy the check costs
    /// one `get_version` round trip.
    pub async fn ensure_running(&self) -> Result<()> {
        if self.ping().await.is_ok() {
            return Ok(());
        }
        tokio::process::Command::new("mpv")
            // The socket path has to match the one this client dials. A
            // hardcoded default was harmless while this only ran at startup;
            // now that it can respawn mpv mid-session, a mismatch would leave
            // an mpv running that nothing can reach.
            .arg(format!("--input-ipc-server={}", self.socket_path.display()))
            .args(["--idle=yes", "--no-terminal"])
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

    /// Sends a JSON command to mpv over its IPC socket and returns the `data`
    /// field of the response, giving up after [`IPC_TIMEOUT`].
    pub async fn send_command(&self, cmd: serde_json::Value) -> Result<serde_json::Value> {
        #[cfg(unix)]
        {
            tokio::time::timeout(self.ipc_timeout, self.exchange(cmd))
                .await
                .map_err(|_| anyhow!("mpv did not answer within {:?}", self.ipc_timeout))?
        }

        #[cfg(not(unix))]
        {
            let _ = cmd;
            Err(anyhow!("mpv IPC is only supported on Unix"))
        }
    }

    /// One request/response exchange on a fresh connection.
    ///
    /// Each command is tagged with a unique `request_id`. mpv also emits
    /// unsolicited event lines on the same connection; those are skipped until
    /// the line whose `request_id` matches the one we sent arrives. This
    /// prevents mistaking an event notification for a command reply.
    ///
    /// Always call this through [`Self::send_command`]: the read loop below
    /// waits as long as mpv takes, which is exactly why it needs the bound
    /// applied there.
    #[cfg(unix)]
    async fn exchange(&self, cmd: serde_json::Value) -> Result<serde_json::Value> {
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

    async fn get_property(&self, name: &str) -> Result<serde_json::Value> {
        self.send_command(serde_json::json!({"command": ["get_property", name]}))
            .await
    }

    /// Loads `url` and plays it on repeat until something else stops it.
    ///
    /// Signage runs unattended, so a video that plays once and leaves the TV
    /// on a black idle screen is never what is wanted — the file loops until
    /// an operator sends stop or plays something else.
    ///
    /// `loop-file` is set *before* `loadfile`, not after: it is a global
    /// property that outlives any one file, and setting it first means even a
    /// very short clip cannot reach its end in the gap between the two
    /// commands. Each `send_command` opens its own connection, so all three
    /// are separate IPC round trips.
    ///
    /// `pause` is cleared *after* `loadfile`, and clearing it is not optional.
    /// Like `loop-file` it is a global property that outlives the file it was
    /// set on, and `stop` does not reset it — so pause, stop, play left the
    /// flag set and the next video loaded and then sat on its first frame,
    /// looking like a dead player until someone pressed space on the Pi.
    /// After rather than before, because setting it while the previous file is
    /// still loaded resumes *that* file for the moment before `loadfile`
    /// replaces it, flashing the old content onto the screen.
    pub async fn play(&self, url: &str) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["set_property", "loop-file", "inf"]}))
            .await?;
        self.send_command(serde_json::json!({"command": ["loadfile", url, "replace"]}))
            .await?;
        self.send_command(serde_json::json!({"command": ["set_property", "pause", false]}))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// Unix socket paths are filesystem entries, so every test needs its own.
    ///
    /// `/tmp` rather than `std::env::temp_dir()`: on macOS that expands to a
    /// long per-user path under `/var/folders`, and a socket path over
    /// `sun_path` (104 bytes there) fails to bind. Keep the name short too —
    /// pid plus a counter is enough to stay unique within a run.
    fn socket_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!("/tmp/tva-{label}-{}-{n}.sock", std::process::id()))
    }

    #[tokio::test]
    async fn send_command_gives_up_when_mpv_never_replies() {
        let path = socket_path("silent");
        let listener = UnixListener::bind(&path).unwrap();
        // A stand-in for a wedged mpv: it accepts the connection and then says
        // nothing at all, which is what a stalled network read looks like from
        // this side. Before the timeout this hung the caller forever.
        let _silent = tokio::spawn(async move {
            let _conn = listener.accept().await;
            std::future::pending::<()>().await;
        });

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_millis(100));
        let err = client
            .send_command(serde_json::json!({"command": ["get_version"]}))
            .await
            .expect_err("a silent socket must not hang the caller");

        assert!(
            err.to_string().contains("did not answer"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn send_command_skips_events_and_returns_the_matching_reply() {
        let path = socket_path("chatty");
        let listener = UnixListener::bind(&path).unwrap();
        let fake_mpv = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = request["request_id"].as_u64().unwrap();

            // An unsolicited event arrives before the reply we asked for.
            write_half
                .write_all(b"{\"event\":\"file-loaded\"}\n")
                .await
                .unwrap();
            let reply =
                serde_json::json!({"data": "0.35.0", "error": "success", "request_id": id});
            write_half
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        });

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        let data = client
            .send_command(serde_json::json!({"command": ["get_version"]}))
            .await
            .unwrap();

        assert_eq!(data, serde_json::json!("0.35.0"));
        fake_mpv.await.unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn send_command_reports_a_missing_socket_rather_than_waiting() {
        let client = MpvClient::with_ipc_timeout(socket_path("absent"), Duration::from_secs(5));
        let err = client
            .send_command(serde_json::json!({"command": ["get_version"]}))
            .await
            .expect_err("there is no socket to connect to");

        // The connect fails immediately, so this is the error a crashed mpv
        // produces — distinct from the timeout above, and worth keeping so.
        assert!(
            err.to_string().contains("connect to mpv socket"),
            "unexpected error: {err}"
        );
    }

    /// A fake mpv that answers every command with success and records what it
    /// was asked. Each `send_command` opens its own connection, so this has to
    /// keep accepting rather than serving a single stream.
    fn recording_mpv(path: PathBuf) -> std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = UnixListener::bind(&path).unwrap();
        let recorded = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let (read_half, mut write_half) = tokio::io::split(stream);
                    let mut lines = BufReader::new(read_half).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let id = req["request_id"].as_u64().unwrap();
                        recorded.lock().unwrap().push(req["command"].clone());
                        let reply =
                            serde_json::json!({"data": null, "error": "success", "request_id": id});
                        let _ = write_half.write_all(format!("{reply}\n").as_bytes()).await;
                    }
                });
            }
        });
        seen
    }

    #[tokio::test]
    async fn play_arms_the_loop_loads_then_clears_pause() {
        let path = socket_path("loop");
        let seen = recording_mpv(path.clone());

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client.play("http://server:8000/videos/promo.mp4").await.unwrap();

        let commands = seen.lock().unwrap().clone();
        assert_eq!(
            commands,
            vec![
                serde_json::json!(["set_property", "loop-file", "inf"]),
                serde_json::json!(["loadfile", "http://server:8000/videos/promo.mp4", "replace"]),
                serde_json::json!(["set_property", "pause", false]),
            ],
            "play must arm the loop before loading (a short clip can otherwise end \
             first) and clear pause after (clearing it before resumes the outgoing \
             file for a moment, flashing old content)"
        );
        let _ = std::fs::remove_file(&path);
    }
}
