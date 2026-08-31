use anyhow::{anyhow, Result};
use shared::{AgentStatus, DeviceState, MediaKind};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

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
    /// Which video the server last told us to play.
    ///
    /// mpv knows the *URL* it loaded, but the dashboard needs the video's id,
    /// and mpv has no idea about those — so the agent remembers the id from
    /// the `PlayCommand` that started this playback. Reported back through
    /// `get_status`, which is what lets the dashboard keep showing a title
    /// after the first heartbeat.
    ///
    /// A `std::sync::Mutex` rather than tokio's: it is only ever held for a
    /// read or a write of one `Option`, never across an await.
    current_video_id: Mutex<Option<Uuid>>,
    /// Display rotation in degrees, remembered so it can be re-applied.
    ///
    /// `video-rotate` is a global mpv property that survives across files, but
    /// not across an mpv *process* — and `ensure_running` respawns mpv
    /// whenever it has died. Without a local copy, a TV that lost its mpv
    /// would come back unrotated and stay that way until someone touched the
    /// dashboard.
    rotation: Mutex<u16>,
    /// URL of the image to display when nothing is playing, if any.
    idle_background: Mutex<Option<String>>,
    /// Whether that image is what is currently on screen.
    ///
    /// mpv cannot tell us this: showing the idle background means a file *is*
    /// loaded, so `idle-active` reports false and the device would look like
    /// it were playing. This flag is what keeps a stopped TV reading as Idle.
    showing_idle_background: Mutex<bool>,
}

impl MpvClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            ipc_timeout: IPC_TIMEOUT,
            current_video_id: Mutex::new(None),
            rotation: Mutex::new(0),
            idle_background: Mutex::new(None),
            showing_idle_background: Mutex::new(false),
        }
    }

    /// The same client with a shorter fuse, so the timeout test does not have
    /// to sleep out a whole [`IPC_TIMEOUT`].
    #[cfg(test)]
    fn with_ipc_timeout(socket_path: impl Into<PathBuf>, ipc_timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            ipc_timeout,
            current_video_id: Mutex::new(None),
            rotation: Mutex::new(0),
            idle_background: Mutex::new(None),
            showing_idle_background: Mutex::new(false),
        }
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
    /// Arm the right "hold on screen forever" mechanism for this media kind.
    ///
    /// A video loops: `loop-file=inf` restarts it at EOF. A still image has no
    /// EOF to loop — mpv gives it `image-display-duration` seconds (1 by
    /// default) and then moves on, leaving a black screen. The two settings are
    /// both global and both outlive the file they were set on, so each play
    /// sets both: switching from an image to a video without clearing
    /// `loop-file` would leave the image's setting armed, and vice versa.
    async fn set_loop_mode(&self, kind: MediaKind) -> Result<()> {
        let (loop_file, image_duration) = match kind {
            MediaKind::Video => ("inf", serde_json::json!(1.0)),
            MediaKind::Image => ("no", serde_json::json!("inf")),
        };
        self.send_command(
            serde_json::json!({"command": ["set_property", "loop-file", loop_file]}),
        )
        .await?;
        self.send_command(serde_json::json!({
            "command": ["set_property", "image-display-duration", image_duration]
        }))
        .await?;
        Ok(())
    }

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
    pub async fn play(&self, url: &str, video_id: Uuid, kind: MediaKind) -> Result<()> {
        // Re-applied on every play rather than only when it is set. mpv keeps
        // `video-rotate` across files but not across processes, and
        // `ensure_running` respawns mpv after a crash — this is what makes a
        // recovered TV come back the right way up.
        self.apply_rotation().await?;
        self.set_loop_mode(kind).await?;
        self.send_command(serde_json::json!({"command": ["loadfile", url, "replace"]}))
            .await?;
        self.send_command(serde_json::json!({"command": ["set_property", "pause", false]}))
            .await?;
        // Recorded only once the commands above succeeded — claiming a video
        // we failed to load would put a title on the dashboard for a TV that
        // is showing nothing.
        *self.current_video_id.lock().expect("video id lock poisoned") = Some(video_id);
        self.set_showing_idle_background(false);
        Ok(())
    }

    /// Stop playback, falling back to the idle background if one is set.
    ///
    /// A bare `stop` leaves mpv idle on a black screen. Where the client has
    /// chosen artwork, a stopped TV should show that instead — a wall of black
    /// rectangles during business hours looks like a fleet of broken screens.
    pub async fn stop(&self) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["stop"]})).await?;
        // pause/resume deliberately leave this alone — only stop and playing
        // something else change what is on screen.
        *self.current_video_id.lock().expect("video id lock poisoned") = None;
        self.set_showing_idle_background(false);

        // Best-effort: failing to paint the background must not turn a
        // successful stop into a failed command. The TV is already stopped.
        if let Err(err) = self.show_idle_background().await {
            tracing::warn!("could not show idle background after stop: {err:#}");
        }
        Ok(())
    }

    /// Load the idle background, if one is configured. A no-op otherwise.
    pub async fn show_idle_background(&self) -> Result<()> {
        let Some(url) = self.idle_background() else {
            return Ok(());
        };

        self.apply_rotation().await?;
        self.set_loop_mode(MediaKind::Image).await?;
        self.send_command(serde_json::json!({"command": ["loadfile", url, "replace"]}))
            .await?;
        self.send_command(serde_json::json!({"command": ["set_property", "pause", false]}))
            .await?;

        // Set only after the load succeeded: claiming the background is up
        // when it is not would report a black TV as a deliberately idle one.
        self.set_showing_idle_background(true);
        Ok(())
    }

    /// Change the idle background and, if the TV is idle, show it immediately.
    pub async fn set_idle_background(&self, url: Option<String>) -> Result<()> {
        let was_showing = self.is_showing_idle_background();
        *self.idle_background.lock().expect("idle background lock poisoned") = url.clone();

        match url {
            // Repaint only when the screen is already idle. Doing it while a
            // video is playing would yank the content off a live TV.
            Some(_) if was_showing || self.current_video_id().is_none() => {
                self.show_idle_background().await
            }
            // Cleared while it is the thing on screen: nothing to fall back
            // to but a black screen, which is what `stop` used to leave.
            None if was_showing => {
                self.send_command(serde_json::json!({"command": ["stop"]})).await?;
                self.set_showing_idle_background(false);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn idle_background(&self) -> Option<String> {
        self.idle_background
            .lock()
            .expect("idle background lock poisoned")
            .clone()
    }

    fn current_video_id(&self) -> Option<Uuid> {
        *self.current_video_id.lock().expect("video id lock poisoned")
    }

    fn is_showing_idle_background(&self) -> bool {
        *self
            .showing_idle_background
            .lock()
            .expect("idle background flag lock poisoned")
    }

    fn set_showing_idle_background(&self, showing: bool) {
        *self
            .showing_idle_background
            .lock()
            .expect("idle background flag lock poisoned") = showing;
    }

    /// Remember a rotation and apply it now.
    pub async fn set_rotation(&self, degrees: u16) -> Result<()> {
        *self.rotation.lock().expect("rotation lock poisoned") = degrees;
        self.apply_rotation().await
    }

    pub fn rotation(&self) -> u16 {
        *self.rotation.lock().expect("rotation lock poisoned")
    }

    /// Push the remembered rotation into mpv.
    async fn apply_rotation(&self) -> Result<()> {
        let degrees = self.rotation();
        self.send_command(
            serde_json::json!({"command": ["set_property", "video-rotate", degrees]}),
        )
        .await?;
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

        // Showing the idle background means a file is loaded, so mpv reports
        // `idle-active` false and every stopped TV would come back as Playing
        // — with the background image's own runtime on the tile. The agent's
        // own flag is the authority here, not mpv.
        let showing_background = self.is_showing_idle_background();

        let state = if idle || showing_background {
            DeviceState::Idle
        } else if paused {
            DeviceState::Paused
        } else {
            DeviceState::Playing
        };

        // mpv is the authority on whether anything is loaded at all. If it
        // went idle behind our back — someone stopped it on the Pi, or it
        // crashed and respawned — our remembered id is stale, and reporting it
        // would leave a title on a tile showing nothing.
        let current_video_id = if idle {
            *self.current_video_id.lock().expect("video id lock poisoned") = None;
            self.set_showing_idle_background(false);
            None
        } else if showing_background {
            // The background is not a library selection; reporting it would
            // put artwork's filename on the tile as if it were playing.
            None
        } else {
            *self.current_video_id.lock().expect("video id lock poisoned")
        };

        Ok(AgentStatus {
            state,
            current_video_id,
            position_secs: if showing_background { None } else { position_secs },
            duration_secs: if showing_background { None } else { duration_secs },
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

    /// A fake mpv that records every command and answers `get_property` from
    /// `props`, so a test can put mpv into a specific state. Anything else
    /// succeeds with a null result. Each `send_command` opens its own
    /// connection, so this keeps accepting rather than serving one stream.
    fn fake_mpv(
        path: PathBuf,
        props: serde_json::Value,
    ) -> std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = UnixListener::bind(&path).unwrap();
        let recorded = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let (recorded, props) = (recorded.clone(), props.clone());
                tokio::spawn(async move {
                    let (read_half, mut write_half) = tokio::io::split(stream);
                    let mut lines = BufReader::new(read_half).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let id = req["request_id"].as_u64().unwrap();
                        let cmd = req["command"].clone();
                        recorded.lock().unwrap().push(cmd.clone());

                        let data = match (cmd[0].as_str(), cmd[1].as_str()) {
                            (Some("get_property"), Some(name)) => {
                                props.get(name).cloned().unwrap_or(serde_json::Value::Null)
                            }
                            _ => serde_json::Value::Null,
                        };
                        let reply =
                            serde_json::json!({"data": data, "error": "success", "request_id": id});
                        let _ = write_half.write_all(format!("{reply}\n").as_bytes()).await;
                    }
                });
            }
        });
        seen
    }

    #[tokio::test]
    async fn a_tv_showing_the_idle_background_still_reports_idle() {
        let path = socket_path("idlebg");
        // mpv reports a file loaded and not paused — which on its own reads as
        // Playing, because the background *is* a loaded file.
        let _seen = fake_mpv(
            path.clone(),
            serde_json::json!({"idle-active": false, "pause": false, "time-pos": 3.0, "duration": 9.0}),
        );

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client
            .set_idle_background(Some("http://server:8000/videos/logo.png".into()))
            .await
            .unwrap();

        let status = client.get_status().await.unwrap();
        assert_eq!(
            status.state,
            DeviceState::Idle,
            "a stopped TV showing artwork is idle, not playing"
        );
        assert_eq!(status.current_video_id, None);
        assert_eq!(
            status.position_secs, None,
            "the background's playhead is not a video position"
        );
    }

    #[tokio::test]
    async fn stop_falls_back_to_the_idle_background() {
        let path = socket_path("stopbg");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client
            .set_idle_background(Some("http://server:8000/videos/logo.png".into()))
            .await
            .unwrap();
        seen.lock().unwrap().clear();

        client.stop().await.unwrap();

        let commands = seen.lock().unwrap().clone();
        assert!(
            commands.contains(&serde_json::json!([
                "loadfile",
                "http://server:8000/videos/logo.png",
                "replace"
            ])),
            "stop should leave the client's artwork on screen, not a black rectangle: {commands:?}"
        );
    }

    #[tokio::test]
    async fn stop_without_a_background_configured_just_stops() {
        let path = socket_path("stopplain");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client.stop().await.unwrap();

        assert_eq!(
            seen.lock().unwrap().clone(),
            vec![serde_json::json!(["stop"])],
            "with nothing configured, stop must not load anything"
        );
    }

    #[tokio::test]
    async fn playing_something_clears_the_idle_background_flag() {
        let path = socket_path("bgthenplay");
        let _seen = fake_mpv(
            path.clone(),
            serde_json::json!({"idle-active": false, "pause": false, "time-pos": 2.0, "duration": 30.0}),
        );

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client
            .set_idle_background(Some("http://server:8000/videos/logo.png".into()))
            .await
            .unwrap();
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
            )
            .await
            .unwrap();

        let status = client.get_status().await.unwrap();
        assert_eq!(
            status.state,
            DeviceState::Playing,
            "the background flag must not survive a real play"
        );
        assert_eq!(status.position_secs, Some(2.0));
    }

    #[tokio::test]
    async fn rotation_is_reapplied_on_every_play() {
        let path = socket_path("rotplay");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client.set_rotation(90).await.unwrap();
        seen.lock().unwrap().clear();

        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
            )
            .await
            .unwrap();

        // mpv keeps video-rotate across files but not across processes, and
        // ensure_running respawns it after a crash — so a play that follows a
        // respawn has to set it again or the TV comes back the wrong way up.
        assert_eq!(
            seen.lock().unwrap().first(),
            Some(&serde_json::json!(["set_property", "video-rotate", 90])),
        );
    }

    #[tokio::test]
    async fn play_holds_an_image_instead_of_looping_it() {
        let path = socket_path("image");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client
            .play(
                "http://server:8000/videos/logo.png",
                Uuid::new_v4(),
                MediaKind::Image,
            )
            .await
            .unwrap();

        let commands = seen.lock().unwrap().clone();
        assert_eq!(
            commands,
            vec![
                serde_json::json!(["set_property", "video-rotate", 0]),
                serde_json::json!(["set_property", "loop-file", "no"]),
                serde_json::json!(["set_property", "image-display-duration", "inf"]),
                serde_json::json!(["loadfile", "http://server:8000/videos/logo.png", "replace"]),
                serde_json::json!(["set_property", "pause", false]),
            ],
            "a still has no end to loop at — without image-display-duration=inf mpv \
             shows it for one second and then leaves a black screen"
        );
    }

    #[tokio::test]
    async fn play_arms_the_loop_loads_then_clears_pause() {
        let path = socket_path("loop");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
            )
            .await
            .unwrap();

        let commands = seen.lock().unwrap().clone();
        assert_eq!(
            commands,
            vec![
                serde_json::json!(["set_property", "video-rotate", 0]),
                serde_json::json!(["set_property", "loop-file", "inf"]),
                serde_json::json!(["set_property", "image-display-duration", 1.0]),
                serde_json::json!(["loadfile", "http://server:8000/videos/promo.mp4", "replace"]),
                serde_json::json!(["set_property", "pause", false]),
            ],
            "play must arm the loop before loading (a short clip can otherwise end \
             first) and clear pause after (clearing it before resumes the outgoing \
             file for a moment, flashing old content)"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn status_reports_the_video_that_play_was_given() {
        let path = socket_path("vid");
        // Not idle and not paused: something is genuinely on screen.
        let _seen = fake_mpv(
            path.clone(),
            serde_json::json!({"idle-active": false, "pause": false}),
        );
        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));
        let video_id = Uuid::new_v4();

        client
            .play("http://server:8000/videos/promo.mp4", video_id, MediaKind::Video)
            .await
            .unwrap();
        let status = client.get_status().await.unwrap();

        // The whole point: the heartbeat overwrites devices.current_video with
        // whatever this says, so a None here wipes the title off the dashboard
        // one poll after playback starts.
        assert_eq!(status.current_video_id, Some(video_id));
        assert_eq!(status.state, DeviceState::Playing);

        client.stop().await.unwrap();
        assert_eq!(
            client.get_status().await.unwrap().current_video_id,
            None,
            "stop must clear the video, not leave a stale title on the tile"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn status_forgets_the_video_when_mpv_went_idle_on_its_own() {
        let path = socket_path("idle");
        // mpv reports nothing loaded — stopped on the Pi, or crashed and
        // respawned. Our remembered id is stale and must not be reported.
        let _seen = fake_mpv(path.clone(), serde_json::json!({"idle-active": true}));
        let client = MpvClient::with_ipc_timeout(&path, Duration::from_secs(5));

        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
            )
            .await
            .unwrap();
        let status = client.get_status().await.unwrap();

        assert_eq!(status.state, DeviceState::Idle);
        assert_eq!(
            status.current_video_id, None,
            "mpv is the authority on whether anything is loaded"
        );
        let _ = std::fs::remove_file(&path);
    }
}
