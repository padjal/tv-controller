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

/// How soon after a synchronised start the first drift check runs.
///
/// Short on purpose. It is what rescues a Pi that buffered slowly and missed
/// the agreed instant: without it that TV stays visibly behind for a whole
/// [`RESYNC_INTERVAL`], which is exactly the stagger this feature exists to
/// remove.
const RESYNC_FIRST: Duration = Duration::from_secs(3);

/// How often playback is checked against the shared timeline afterwards.
///
/// Each TV loops on its own crystal, so even a perfect start decays over a
/// long unattended run. This is the correction for that, and it is deliberately
/// infrequent — the drift being corrected accumulates over minutes.
const RESYNC_INTERVAL: Duration = Duration::from_secs(30);

/// Drift below this is left alone. Well under a frame at 25fps, and chasing it
/// would mean permanently nudging the speed back and forth.
const DRIFT_DEADBAND: f64 = 0.05;

/// Drift at or above this is corrected by seeking rather than by speed.
///
/// A seek re-buffers an HTTP stream and is visible; the speed nudge is not.
/// But a nudge closes a gap slowly, so a large one has to be cut.
const DRIFT_SEEK_THRESHOLD: f64 = 1.0;

/// How much the playback rate is trimmed to absorb small drift. 2% is inaudible
/// (mpv corrects pitch by default) and invisible.
const DRIFT_NUDGE: f64 = 0.02;

/// Wall-clock milliseconds — the clock the whole fleet agrees on.
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// How far `position` is ahead of `canonical`, taking the shorter way round a
/// loop of length `duration`.
///
/// Both are points on a circle: a file that has just wrapped sits near 0 while
/// the timeline still says near `duration`, which read naively is a whole
/// duration of drift and would trigger a pointless seek on every loop. The
/// result is signed — positive means ahead — and never exceeds half a loop.
fn shortest_drift(position: f64, canonical: f64, duration: f64) -> f64 {
    let mut drift = position - canonical;
    if drift > duration / 2.0 {
        drift -= duration;
    } else if drift < -duration / 2.0 {
        drift += duration;
    }
    drift
}

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
    /// Bumped by anything that changes what is on screen.
    ///
    /// A synchronised start waits before releasing the pause, and a drift
    /// correction loop runs for as long as that playback lasts. Both capture
    /// the value at the moment they were created and stop as soon as it moves,
    /// so a second play cannot have its pause cleared by the task belonging to
    /// the first — which would unpause the *new* file at the old file's
    /// moment, or seek it to a position on the wrong timeline.
    generation: AtomicU64,
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
            generation: AtomicU64::new(0),
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
            generation: AtomicU64::new(0),
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
    pub async fn play(
        self: &std::sync::Arc<Self>,
        url: &str,
        video_id: Uuid,
        kind: MediaKind,
        start_at_unix_ms: Option<i64>,
    ) -> Result<()> {
        // Re-applied on every play rather than only when it is set. mpv keeps
        // `video-rotate` across files but not across processes, and
        // `ensure_running` respawns mpv after a crash — this is what makes a
        // recovered TV come back the right way up.
        self.apply_rotation().await?;
        self.set_loop_mode(kind).await?;

        // Supersede any start or drift correction still pending from the last
        // play before touching what is on screen.
        let generation = self.next_generation();

        // Speed is global and outlives the file, like pause and loop-file. A
        // previous correction could otherwise leave this file running at 1.02.
        self.set_speed(1.0).await?;

        // Load paused so that every TV can be released at the same instant
        // rather than whenever each finished buffering. Pausing before the
        // load is safe: the hazard the ordering below guards against is
        // *resuming* the outgoing file, which pausing cannot do.
        if start_at_unix_ms.is_some() {
            self.send_command(serde_json::json!({"command": ["set_property", "pause", true]}))
                .await?;
        }

        self.send_command(serde_json::json!({"command": ["loadfile", url, "replace"]}))
            .await?;

        match start_at_unix_ms {
            None => {
                self.send_command(
                    serde_json::json!({"command": ["set_property", "pause", false]}),
                )
                .await?;
            }
            Some(start_at) => {
                // Released by a background task so the handler can answer now:
                // the server gives a play five seconds, and the lead time
                // alone would eat most of that.
                let client = std::sync::Arc::clone(self);
                tokio::spawn(async move {
                    client.run_synchronised(generation, start_at, kind).await;
                });
            }
        }

        // Recorded only once the commands above succeeded — claiming a video
        // we failed to load would put a title on the dashboard for a TV that
        // is showing nothing.
        *self.current_video_id.lock().expect("video id lock poisoned") = Some(video_id);
        self.set_showing_idle_background(false);
        Ok(())
    }

    /// Hold the loaded file until `start_at`, then keep it on the shared
    /// timeline for as long as this playback lasts.
    async fn run_synchronised(&self, generation: u64, start_at: i64, kind: MediaKind) {
        let wait = start_at.saturating_sub(now_unix_ms());
        if wait > 0 {
            tokio::time::sleep(Duration::from_millis(wait as u64)).await;
        }
        if self.generation() != generation {
            return;
        }

        // Late arrival: the agreed moment has already passed, so start from
        // where the others are rather than from the beginning.
        if let Err(err) = self.correct_drift(start_at, kind).await {
            tracing::debug!("could not align before start: {err:#}");
        }

        if let Err(err) = self
            .send_command(serde_json::json!({"command": ["set_property", "pause", false]}))
            .await
        {
            tracing::warn!("synchronised start failed to unpause: {err:#}");
            return;
        }

        // A still has no timeline to drift along.
        if kind.is_image() {
            return;
        }

        let mut delay = RESYNC_FIRST;
        loop {
            tokio::time::sleep(delay).await;
            delay = RESYNC_INTERVAL;
            if self.generation() != generation {
                return;
            }
            if let Err(err) = self.correct_drift(start_at, kind).await {
                tracing::debug!("drift check failed: {err:#}");
            }
        }
    }

    /// Pull playback back towards where the shared timeline says it should be.
    ///
    /// The canonical position is `((now - epoch) / 1000) mod duration`: every
    /// TV given the same epoch computes the same answer, so they converge on
    /// each other without talking to one another — and a TV that joins late,
    /// or was restarted, lands in the right place rather than at zero.
    async fn correct_drift(&self, epoch_ms: i64, kind: MediaKind) -> Result<()> {
        if kind.is_image() {
            return Ok(());
        }

        let duration = self
            .get_property("duration")
            .await?
            .as_f64()
            .filter(|d| d.is_finite() && *d > 0.0)
            .ok_or_else(|| anyhow!("mpv reported no duration"))?;
        let position = self
            .get_property("time-pos")
            .await?
            .as_f64()
            .ok_or_else(|| anyhow!("mpv reported no position"))?;

        let elapsed = (now_unix_ms() - epoch_ms) as f64 / 1000.0;
        if elapsed < 0.0 {
            return Ok(());
        }
        let canonical = elapsed % duration;

        let drift = shortest_drift(position, canonical, duration);

        if drift.abs() < DRIFT_DEADBAND {
            self.set_speed(1.0).await?;
        } else if drift.abs() >= DRIFT_SEEK_THRESHOLD {
            // Too far to nudge out. Visible, but so is the gap.
            self.set_speed(1.0).await?;
            self.send_command(serde_json::json!({
                "command": ["seek", canonical, "absolute"]
            }))
            .await?;
            tracing::info!(drift, canonical, "seeked to correct playback drift");
        } else {
            // Ahead runs slow, behind runs fast, until the next check finds it
            // inside the deadband and restores 1.0.
            let speed = if drift > 0.0 { 1.0 - DRIFT_NUDGE } else { 1.0 + DRIFT_NUDGE };
            self.set_speed(speed).await?;
            tracing::debug!(drift, speed, "nudging playback rate to correct drift");
        }

        Ok(())
    }

    async fn set_speed(&self, speed: f64) -> Result<()> {
        self.send_command(serde_json::json!({"command": ["set_property", "speed", speed]}))
            .await?;
        Ok(())
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Stop playback, falling back to the idle background if one is set.
    ///
    /// A bare `stop` leaves mpv idle on a black screen. Where the client has
    /// chosen artwork, a stopped TV should show that instead — a wall of black
    /// rectangles during business hours looks like a fleet of broken screens.
    pub async fn stop(&self) -> Result<()> {
        // Cancels a scheduled start that has not fired yet, and any drift
        // correction still running for the file being stopped.
        self.next_generation();
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .set_idle_background(Some("http://server:8000/videos/logo.png".into()))
            .await
            .unwrap();
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                None,
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

    #[test]
    fn drift_is_measured_the_shorter_way_round_a_loop() {
        // Straightforward cases: ahead is positive, behind is negative.
        assert!((shortest_drift(12.0, 10.0, 30.0) - 2.0).abs() < 1e-9);
        assert!((shortest_drift(8.0, 10.0, 30.0) + 2.0).abs() < 1e-9);

        // Just wrapped: 0.5s into the file while the timeline says 29.5s is
        // one second ahead, not 29 seconds behind. Read naively this would
        // seek on every single loop.
        assert!((shortest_drift(0.5, 29.5, 30.0) - 1.0).abs() < 1e-9);

        // And the mirror image: about to wrap, timeline already has.
        assert!((shortest_drift(29.5, 0.5, 30.0) + 1.0).abs() < 1e-9);

        // Never reports more than half a loop in either direction.
        for (pos, canon) in [(0.0, 15.0), (15.0, 0.0), (29.9, 0.1), (0.1, 29.9)] {
            assert!(shortest_drift(pos, canon, 30.0).abs() <= 15.0 + 1e-9);
        }
    }

    #[tokio::test]
    async fn a_scheduled_play_loads_paused_and_does_not_start_early() {
        let path = socket_path("sched");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        // Far enough out that the release cannot have happened yet.
        let start_at = now_unix_ms() + 10_000;
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                Some(start_at),
            )
            .await
            .unwrap();

        let commands = seen.lock().unwrap().clone();
        assert!(
            commands.contains(&serde_json::json!(["set_property", "pause", true])),
            "a synchronised play must load paused so every TV can be released together: {commands:?}"
        );
        assert!(
            !commands.contains(&serde_json::json!(["set_property", "pause", false])),
            "nothing may be unpaused before the agreed instant: {commands:?}"
        );

        // The pause must come before the load: releasing it is what starts the
        // file, so a load that is not already paused has already begun.
        let pause_at = commands
            .iter()
            .position(|c| c == &serde_json::json!(["set_property", "pause", true]))
            .unwrap();
        let load_at = commands
            .iter()
            .position(|c| c[0] == serde_json::json!("loadfile"))
            .unwrap();
        assert!(pause_at < load_at, "must be paused before loading: {commands:?}");
    }

    #[tokio::test]
    async fn a_scheduled_play_releases_at_the_agreed_moment() {
        let path = socket_path("release");
        // No duration, so the drift check gives up and only the release shows.
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                Some(now_unix_ms() + 150),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;
        let commands = seen.lock().unwrap().clone();
        assert!(
            commands.contains(&serde_json::json!(["set_property", "pause", false])),
            "the scheduled start should have fired by now: {commands:?}"
        );
    }

    /// A superseded start must not fire. Otherwise the task belonging to the
    /// first play unpauses the *second* file at the first one's moment.
    #[tokio::test]
    async fn a_second_play_cancels_the_first_scheduled_start() {
        let path = socket_path("supersede");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .play(
                "http://server:8000/videos/first.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                Some(now_unix_ms() + 200),
            )
            .await
            .unwrap();

        // Replaced immediately by an unscheduled play, which starts at once.
        client
            .play(
                "http://server:8000/videos/second.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                None,
            )
            .await
            .unwrap();

        seen.lock().unwrap().clear();
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert!(
            seen.lock().unwrap().is_empty(),
            "the cancelled start still issued commands: {:?}",
            seen.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn stop_cancels_a_pending_scheduled_start() {
        let path = socket_path("stopsched");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                Some(now_unix_ms() + 200),
            )
            .await
            .unwrap();
        client.stop().await.unwrap();

        seen.lock().unwrap().clear();
        tokio::time::sleep(Duration::from_millis(500)).await;

        // A start firing after a stop would put the TV back on a video the
        // operator had just turned off.
        assert!(
            seen.lock().unwrap().is_empty(),
            "a stopped TV was restarted by a pending schedule: {:?}",
            seen.lock().unwrap()
        );
    }

    /// A TV that missed the agreed instant joins where the others already are,
    /// rather than starting from zero and staying a fixed gap behind forever.
    #[tokio::test]
    async fn a_late_start_seeks_onto_the_shared_timeline() {
        let path = socket_path("late");
        let seen = fake_mpv(
            path.clone(),
            serde_json::json!({"duration": 30.0, "time-pos": 0.0}),
        );

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        // The epoch is already 10s in the past.
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                Some(now_unix_ms() - 10_000),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(400)).await;
        let commands = seen.lock().unwrap().clone();
        let seek = commands.iter().find(|c| c[0] == serde_json::json!("seek"));
        let seek = seek.unwrap_or_else(|| panic!("expected a corrective seek: {commands:?}"));

        let target = seek[1].as_f64().unwrap();
        assert!(
            (target - 10.0).abs() < 1.0,
            "should seek to roughly 10s into a 30s loop, got {target}"
        );
    }

    #[tokio::test]
    async fn an_image_is_scheduled_but_never_drift_corrected() {
        let path = socket_path("imgsync");
        let seen = fake_mpv(
            path.clone(),
            serde_json::json!({"duration": 30.0, "time-pos": 0.0}),
        );

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .play(
                "http://server:8000/videos/logo.png",
                Uuid::new_v4(),
                MediaKind::Image,
                Some(now_unix_ms() - 10_000),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(400)).await;
        let commands = seen.lock().unwrap().clone();
        assert!(
            commands.contains(&serde_json::json!(["set_property", "pause", false])),
            "the image should still be released: {commands:?}"
        );
        // A still has no timeline; seeking one is meaningless and mpv's
        // reported duration for it cannot be trusted.
        assert!(
            !commands.iter().any(|c| c[0] == serde_json::json!("seek")),
            "an image must never be seeked: {commands:?}"
        );
    }

    #[tokio::test]
    async fn rotation_is_reapplied_on_every_play() {
        let path = socket_path("rotplay");
        let seen = fake_mpv(path.clone(), serde_json::json!({}));

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client.set_rotation(90).await.unwrap();
        seen.lock().unwrap().clear();

        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                None,
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .play(
                "http://server:8000/videos/logo.png",
                Uuid::new_v4(),
                MediaKind::Image,
                None,
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
                serde_json::json!(["set_property", "speed", 1.0]),
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

        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                None,
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
                serde_json::json!(["set_property", "speed", 1.0]),
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
        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));
        let video_id = Uuid::new_v4();

        client
            .play(
                "http://server:8000/videos/promo.mp4",
                video_id,
                MediaKind::Video,
                None,
            )
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
        let client = std::sync::Arc::new(MpvClient::with_ipc_timeout(&path, Duration::from_secs(5)));

        client
            .play(
                "http://server:8000/videos/promo.mp4",
                Uuid::new_v4(),
                MediaKind::Video,
                None,
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
