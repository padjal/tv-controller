//! Keeps the `videos` table in sync with the contents of `videos_dir`.
//!
//! Scans once at startup, then watches the directory and rescans after changes
//! settle. The scan is deliberately non-recursive: `filename` is UNIQUE and the
//! serving route is `/videos/:filename`, so a flat directory is the only layout
//! that round-trips.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use shared::SseKind;
use tokio::time::timeout;

use crate::db;
use crate::state::AppState;

const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "mkv", "mov", "avi", "webm"];

/// How long the directory must be quiet before a rescan runs. Copying a large
/// file in produces a burst of events, and probing it mid-copy reads a
/// truncated file — waiting for the burst to end avoids both.
const DEBOUNCE: Duration = Duration::from_secs(2);

/// ffprobe on a large file is fast, but a corrupt or network-stalled file can
/// hang it indefinitely. One slow file must not stall the whole scan.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Set once ffprobe is found to be missing, so the warning is logged one time
/// rather than once per file per scan.
static PROBE_MISSING_LOGGED: AtomicBool = AtomicBool::new(false);

/// Same idea for ffmpeg, which generates the poster frames.
static FFMPEG_MISSING_LOGGED: AtomicBool = AtomicBool::new(false);

/// Width of a generated poster frame, in pixels. Height follows the source
/// aspect ratio, so portrait signage is not squashed into landscape.
///
/// 480 is comfortably more than the dashboard renders (a library row shows it
/// at ~80px, a tile at ~200px) which leaves room for a HiDPI screen without
/// storing anything close to a full frame.
const THUMBNAIL_WIDTH: u32 = 480;

/// ffmpeg decoding a single frame is quick, but the same corrupt or stalled
/// file that can hang ffprobe can hang this. One bad file must not stall the
/// scan.
const THUMBNAIL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    /// Files inserted or whose metadata changed.
    pub upserted: usize,
    /// Rows removed because the file is gone from disk.
    pub pruned: usize,
    /// Files present and already up to date.
    pub unchanged: usize,
}

impl ScanSummary {
    pub fn changed(&self) -> bool {
        self.upserted > 0 || self.pruned > 0
    }
}

/// Walk `videos_dir` once and reconcile it with the database.
pub async fn scan_once(state: &AppState) -> Result<ScanSummary> {
    let dir = &state.videos_dir;
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create videos dir {}", dir.display()))?;

    tokio::fs::create_dir_all(&state.thumbnails_dir)
        .await
        .with_context(|| {
            format!(
                "failed to create thumbnails dir {}",
                state.thumbnails_dir.display()
            )
        })?;

    let mut summary = ScanSummary::default();
    let mut seen: HashSet<String> = HashSet::new();

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to read videos dir {}", dir.display()))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .with_context(|| format!("failed to read videos dir {}", dir.display()))?
    {
        let path = entry.path();
        if !is_video_file(&path) {
            continue;
        }

        // Non-UTF-8 filenames cannot round-trip through a URL; skip loudly
        // rather than storing something the agents could never fetch.
        let Some(filename) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            tracing::warn!(path = %path.display(), "skipping video with non-UTF-8 filename");
            continue;
        };

        let metadata = match entry.metadata().await {
            Ok(m) if m.is_file() => m,
            Ok(_) => continue,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "skipping unreadable video");
                continue;
            }
        };
        let size = metadata.len();

        seen.insert(filename.clone());

        // An unchanged size means an unchanged file, so skip the ffprobe call.
        if db::video_size(&state.db, &filename).await? == Some(size) {
            summary.unchanged += 1;
            continue;
        }

        let duration = probe_duration_secs(&path).await;
        let path_str = path.to_string_lossy().into_owned();
        db::upsert_video(&state.db, &filename, &path_str, size, duration).await?;

        // Only on upsert, which the size check above already limits to new or
        // changed files — so a steady library regenerates nothing, and a file
        // that was replaced in place gets a poster matching its new contents.
        generate_thumbnail(&path, &thumbnail_path(state, &filename), duration).await;

        summary.upserted += 1;
        tracing::info!(%filename, size, ?duration, "indexed video");
    }

    // Drop rows whose file is gone; otherwise the dashboard keeps offering a
    // video that would 404 on the agent.
    for filename in db::list_video_filenames(&state.db).await? {
        if !seen.contains(&filename) && db::delete_video_by_filename(&state.db, &filename).await? {
            // Best-effort: a leftover poster is harmless clutter, and nothing
            // references it once the row is gone.
            let _ = tokio::fs::remove_file(thumbnail_path(state, &filename)).await;
            summary.pruned += 1;
            tracing::info!(%filename, "removed video, file no longer on disk");
        }
    }

    Ok(summary)
}

/// Scan once, then watch `videos_dir` and rescan whenever it changes.
///
/// Runs until the watcher stops; intended to be `tokio::spawn`ed at startup.
pub async fn watch(state: Arc<AppState>) -> Result<()> {
    let summary = scan_once(&state).await?;
    tracing::info!(?summary, "initial video scan complete");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // The notify callback runs on its own thread; an unbounded send never
    // blocks it and never needs a tokio context.
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("failed to create filesystem watcher")?;

    watcher
        .watch(&state.videos_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("failed to watch {}", state.videos_dir.display()))?;
    tracing::info!(dir = %state.videos_dir.display(), "watching videos dir");

    while rx.recv().await.is_some() {
        // Coalesce the burst: keep draining until the directory goes quiet.
        while let Ok(Some(_)) = timeout(DEBOUNCE, rx.recv()).await {}

        match scan_once(&state).await {
            Ok(summary) if summary.changed() => {
                tracing::info!(?summary, "video library changed");
                state.broadcast(
                    SseKind::VideoLibraryChanged,
                    &serde_json::json!({
                        "upserted": summary.upserted,
                        "pruned": summary.pruned,
                    }),
                );
            }
            // A rescan that changed nothing is not worth waking the dashboard.
            Ok(_) => {}
            Err(err) => tracing::error!(%err, "video rescan failed"),
        }
    }

    Ok(())
}

fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}

/// Where the poster frame for `filename` lives.
///
/// Keyed on the filename rather than the video's id so that both the library
/// and a TV tile can build the URL from what they already hold — `devices`
/// stores `current_video` as a filename, not an id, so an id-keyed name would
/// force every tile to resolve one first. `filename` is UNIQUE and comes from a
/// flat directory listing, so it cannot collide or contain a path separator.
pub fn thumbnail_path(state: &AppState, filename: &str) -> std::path::PathBuf {
    state.thumbnails_dir.join(format!("{filename}.jpg"))
}

/// Extract one representative frame from `path` into `dest` as a JPEG.
///
/// Never fatal: a video with no poster still lists and plays, the dashboard
/// just falls back to showing its name alone.
async fn generate_thumbnail(path: &Path, dest: &Path, duration_secs: Option<u32>) {
    // 10% in rather than the first frame. Signage clips routinely open on a
    // fade from black or a blank title card, and a grid of black rectangles is
    // worse than no thumbnails at all. Capped so a long file does not seek
    // minutes in, floored so a very short one does not land inside the fade.
    let seek = duration_secs.map_or(1.0, |d| f64::from(d) * 0.1).clamp(1.0, 10.0);

    match run_ffmpeg(path, dest, seek).await {
        Ok(true) => tracing::debug!(dest = %dest.display(), "generated thumbnail"),
        Ok(false) => {
            // Seeking past the end yields no frame and no file. That happens
            // for anything shorter than the floor above, and for a file whose
            // duration ffprobe could not read — so fall back to frame zero,
            // which always exists.
            match run_ffmpeg(path, dest, 0.0).await {
                Ok(true) => tracing::debug!(dest = %dest.display(), "generated thumbnail at 0s"),
                _ => tracing::warn!(
                    path = %path.display(),
                    "ffmpeg could not extract a thumbnail"
                ),
            }
        }
        Err(()) => {}
    }
}

/// One ffmpeg invocation. `Ok(true)` wrote a frame, `Ok(false)` ran but
/// produced none, `Err(())` means ffmpeg could not be run at all — already
/// logged, and not worth retrying.
async fn run_ffmpeg(path: &Path, dest: &Path, seek: f64) -> Result<bool, ()> {
    let output = tokio::process::Command::new("ffmpeg")
        // -ss before -i seeks by keyframe without decoding everything leading
        // up to it, which is the difference between instant and minutes on a
        // long file.
        .args(["-nostdin", "-v", "error", "-ss", &format!("{seek:.2}")])
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={THUMBNAIL_WIDTH}:-2"),
            "-q:v",
            "4",
            "-y",
        ])
        .arg(dest)
        .output();

    match timeout(THUMBNAIL_TIMEOUT, output).await {
        // ffmpeg exits 0 having written nothing when the seek lands past the
        // end, so the file has to be checked rather than trusting the status.
        Ok(Ok(out)) if out.status.success() => Ok(dest.is_file()),
        Ok(Ok(out)) => {
            tracing::debug!(
                path = %path.display(),
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "ffmpeg exited non-zero"
            );
            Ok(false)
        }
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            if !FFMPEG_MISSING_LOGGED.swap(true, Ordering::Relaxed) {
                tracing::warn!("ffmpeg not found on PATH; videos will be listed without thumbnails");
            }
            Err(())
        }
        Ok(Err(err)) => {
            tracing::warn!(path = %path.display(), %err, "ffmpeg failed to run");
            Err(())
        }
        Err(_) => {
            tracing::warn!(path = %path.display(), "ffmpeg timed out generating a thumbnail");
            Err(())
        }
    }
}

/// Duration in whole seconds via ffprobe, or `None` if it cannot be determined.
///
/// A missing duration is never fatal: the video is still listed and playable,
/// it just shows no runtime in the dashboard.
async fn probe_duration_secs(path: &Path) -> Option<u32> {
    let output = tokio::process::Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_format"])
        .arg(path)
        .output();

    let output = match timeout(PROBE_TIMEOUT, output).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            if !PROBE_MISSING_LOGGED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "ffprobe not found on PATH; videos will be indexed without durations"
                );
            }
            return None;
        }
        Ok(Err(err)) => {
            tracing::warn!(path = %path.display(), %err, "ffprobe failed to run");
            return None;
        }
        Err(_) => {
            tracing::warn!(path = %path.display(), "ffprobe timed out");
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!(path = %path.display(), status = ?output.status, "ffprobe returned an error");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let duration = parse_duration_secs(&stdout);
    if duration.is_none() {
        tracing::debug!(path = %path.display(), "ffprobe reported no usable duration");
    }
    duration
}

/// Pull `format.duration` out of ffprobe's JSON.
///
/// ffprobe emits duration as a *string* ("30.024000"), and omits it or reports
/// "N/A" for formats that do not carry one.
fn parse_duration_secs(json: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let duration = value.get("format")?.get("duration")?;

    let secs = match duration {
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        serde_json::Value::Number(n) => n.as_f64()?,
        _ => return None,
    };

    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(secs.round().min(f64::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Duration parsing ────────────────────────────────────────────────────

    #[test]
    fn parses_duration_from_real_ffprobe_output() {
        // Trimmed from actual `ffprobe -print_format json -show_format` output.
        let json = r#"{
            "format": {
                "filename": "test.mp4",
                "nb_streams": 2,
                "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
                "start_time": "0.000000",
                "duration": "30.024000",
                "size": "1055736",
                "bit_rate": "281263",
                "probe_score": 100
            }
        }"#;
        assert_eq!(parse_duration_secs(json), Some(30));
    }

    #[test]
    fn duration_rounds_to_nearest_second() {
        let mk = |d: &str| format!(r#"{{"format":{{"duration":"{d}"}}}}"#);
        assert_eq!(parse_duration_secs(&mk("30.6")), Some(31));
        assert_eq!(parse_duration_secs(&mk("30.4")), Some(30));
        assert_eq!(parse_duration_secs(&mk("0.2")), Some(0));
    }

    #[test]
    fn duration_accepts_a_bare_number_too() {
        assert_eq!(
            parse_duration_secs(r#"{"format":{"duration":45}}"#),
            Some(45)
        );
    }

    #[test]
    fn missing_or_unusable_duration_is_none() {
        // Formats like some .mkv omit duration entirely.
        assert_eq!(parse_duration_secs(r#"{"format":{"size":"123"}}"#), None);
        assert_eq!(
            parse_duration_secs(r#"{"format":{"duration":"N/A"}}"#),
            None
        );
        assert_eq!(
            parse_duration_secs(r#"{"format":{"duration":"-1.0"}}"#),
            None
        );
        assert_eq!(parse_duration_secs(r#"{}"#), None);
        assert_eq!(parse_duration_secs("not json at all"), None);
        assert_eq!(parse_duration_secs(""), None);
    }

    // ── Extension matching ──────────────────────────────────────────────────

    #[test]
    fn recognises_video_extensions_case_insensitively() {
        for name in [
            "a.mp4", "a.mkv", "a.mov", "a.avi", "a.webm", "a.MP4", "a.MkV",
        ] {
            assert!(is_video_file(Path::new(name)), "{name} should be a video");
        }
        for name in ["a.txt", "a.mp3", "a.mp4.part", "a", "a.", ".mp4"] {
            assert!(
                !is_video_file(Path::new(name)),
                "{name} should not be a video"
            );
        }
    }

    // ── Scanning ────────────────────────────────────────────────────────────

    struct TempDirs {
        root: PathBuf,
        state: Arc<AppState>,
    }

    impl TempDirs {
        async fn new() -> TempDirs {
            let root =
                std::env::temp_dir().join(format!("tv-controller-scan-{}", uuid::Uuid::new_v4()));
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
            TempDirs { root, state }
        }

        fn write(&self, name: &str, bytes: usize) {
            std::fs::write(self.state.videos_dir.join(name), vec![0u8; bytes]).unwrap();
        }

        fn remove(&self, name: &str) {
            std::fs::remove_file(self.state.videos_dir.join(name)).unwrap();
        }

        /// A real, decodable video, as opposed to the zero-filled placeholders
        /// the other tests use — ffmpeg has to be able to pull a frame out of
        /// it. Returns false when ffmpeg is unavailable so the caller can skip.
        fn real_video(&self, name: &str, seconds: u32) -> bool {
            let status = std::process::Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("testsrc=size=320x240:rate=10:duration={seconds}"))
                .args(["-pix_fmt", "yuv420p", "-y"])
                .arg(self.state.videos_dir.join(name))
                .status();
            matches!(status, Ok(s) if s.success())
        }
    }

    /// Thumbnails are a best-effort feature built on an optional binary, the
    /// same way durations are, so their tests skip rather than fail where
    /// ffmpeg is not installed.
    fn ffmpeg_missing() -> bool {
        !matches!(
            std::process::Command::new("ffmpeg").arg("-version").output(),
            Ok(out) if out.status.success()
        )
    }

    impl TempDirs {
    }

    impl Drop for TempDirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn scan_indexes_only_video_files() {
        let t = TempDirs::new().await;
        t.write("clip.mp4", 10);
        t.write("show.mkv", 20);
        t.write("notes.txt", 5);
        t.write("partial.mp4.part", 5);

        let summary = scan_once(&t.state).await.unwrap();
        assert_eq!(summary.upserted, 2);
        assert!(summary.changed());

        let mut names: Vec<_> = db::list_videos(&t.state.db)
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.filename)
            .collect();
        names.sort();
        assert_eq!(names, ["clip.mp4", "show.mkv"]);
    }

    #[tokio::test]
    async fn rescanning_unchanged_files_is_a_no_op() {
        let t = TempDirs::new().await;
        t.write("clip.mp4", 10);
        assert_eq!(scan_once(&t.state).await.unwrap().upserted, 1);

        let second = scan_once(&t.state).await.unwrap();
        assert_eq!(second.upserted, 0);
        assert_eq!(second.unchanged, 1);
        assert!(
            !second.changed(),
            "no-op scan must not notify the dashboard"
        );
    }

    #[tokio::test]
    async fn a_resized_file_is_reindexed_keeping_its_id() {
        let t = TempDirs::new().await;
        t.write("clip.mp4", 10);
        scan_once(&t.state).await.unwrap();
        let before = db::get_video_by_filename(&t.state.db, "clip.mp4")
            .await
            .unwrap()
            .unwrap();

        t.write("clip.mp4", 40);
        let summary = scan_once(&t.state).await.unwrap();
        assert_eq!(summary.upserted, 1);

        let after = db::get_video_by_filename(&t.state.db, "clip.mp4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.id, before.id);
        assert_eq!(after.size_bytes, 40);
    }

    #[tokio::test]
    async fn deleted_files_are_pruned_from_the_library() {
        let t = TempDirs::new().await;
        t.write("keep.mp4", 10);
        t.write("gone.mp4", 10);
        scan_once(&t.state).await.unwrap();
        assert_eq!(db::list_videos(&t.state.db).await.unwrap().len(), 2);

        t.remove("gone.mp4");
        let summary = scan_once(&t.state).await.unwrap();
        assert_eq!(summary.pruned, 1);
        assert_eq!(summary.unchanged, 1);
        assert!(summary.changed());

        let names: Vec<_> = db::list_videos(&t.state.db)
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.filename)
            .collect();
        assert_eq!(names, ["keep.mp4"]);
    }

    #[tokio::test]
    async fn scan_creates_a_missing_videos_dir() {
        let t = TempDirs::new().await;
        std::fs::remove_dir_all(&t.state.videos_dir).unwrap();

        let summary = scan_once(&t.state).await.unwrap();
        assert_eq!(summary, ScanSummary::default());
        assert!(t.state.videos_dir.is_dir());
    }

    #[tokio::test]
    async fn subdirectories_are_ignored() {
        let t = TempDirs::new().await;
        let nested = t.state.videos_dir.join("nested.mp4");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("inner.mp4"), b"x").unwrap();

        let summary = scan_once(&t.state).await.unwrap();
        assert_eq!(summary.upserted, 0);
        assert!(db::list_videos(&t.state.db).await.unwrap().is_empty());
    }

    // ── Thumbnails ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn scan_generates_a_thumbnail_and_prunes_it_with_the_video() {
        if ffmpeg_missing() {
            return;
        }
        let t = TempDirs::new().await;
        assert!(t.real_video("clip.mp4", 3), "could not synthesise a video");

        scan_once(&t.state).await.unwrap();

        let thumb = thumbnail_path(&t.state, "clip.mp4");
        assert!(thumb.is_file(), "no thumbnail at {}", thumb.display());
        assert!(
            std::fs::metadata(&thumb).unwrap().len() > 0,
            "thumbnail is empty"
        );

        // Removing the video must take its poster with it, or the directory
        // grows forever as the library turns over.
        t.remove("clip.mp4");
        let summary = scan_once(&t.state).await.unwrap();
        assert_eq!(summary.pruned, 1);
        assert!(!thumb.exists(), "thumbnail outlived its video");
    }

    #[tokio::test]
    async fn a_clip_shorter_than_the_seek_floor_still_gets_a_thumbnail() {
        if ffmpeg_missing() {
            return;
        }
        let t = TempDirs::new().await;
        // Under the 1s floor `generate_thumbnail` clamps to, so the first
        // ffmpeg pass seeks past the end and writes nothing. Only the retry at
        // frame zero produces a poster.
        assert!(t.real_video("blink.mp4", 1), "could not synthesise a video");

        scan_once(&t.state).await.unwrap();

        assert!(
            thumbnail_path(&t.state, "blink.mp4").is_file(),
            "the fallback to frame zero did not produce a thumbnail"
        );
    }

    #[tokio::test]
    async fn an_undecodable_file_is_still_indexed_without_a_thumbnail() {
        let t = TempDirs::new().await;
        // Zero-filled: ffmpeg can make nothing of it. The video must still
        // list and play — a missing poster is cosmetic.
        t.write("junk.mp4", 64);

        let summary = scan_once(&t.state).await.unwrap();

        assert_eq!(summary.upserted, 1);
        assert!(!thumbnail_path(&t.state, "junk.mp4").exists());
    }
}
