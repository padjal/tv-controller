import { useCallback, useEffect, useId, useState } from "react";
import { api, EVENTS_URL } from "../api";
import { useSSE } from "../hooks/useSSE";
import type { Settings, Video } from "../types";
import { Thumbnail } from "./Thumbnail";
import { VideoPreview } from "./VideoPreview";
import "./VideoLibrary.css";

export interface VideoLibraryProps {
  selectedVideoId: string | null;
  /** `null` clears the selection — used when the selected file disappears. */
  onSelect: (videoId: string | null) => void;
}

/**
 * `m:ss`, or `h:mm:ss` past an hour.
 *
 * Duration is null whenever ffprobe was unavailable or the container carries
 * no duration, which is a supported state rather than an error.
 */
export function formatDuration(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) {
    return "—";
  }
  const total = Math.round(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;

  const padded = String(secs).padStart(2, "0");
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${padded}`
    : `${minutes}:${padded}`;
}

export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Whole bytes read oddly with a decimal point.
  return unit === 0 ? `${value} ${units[unit]}` : `${value.toFixed(1)} ${units[unit]}`;
}

export function VideoLibrary({ selectedVideoId, onSelect }: VideoLibraryProps) {
  const [videos, setVideos] = useState<Video[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [idleBackground, setIdleBackground] = useState<string | null>(null);
  const [backgroundError, setBackgroundError] = useState<string | null>(null);
  // Radios need a shared name that is unique to this instance.
  const groupName = useId();
  const backgroundId = useId();

  const refresh = useCallback(() => {
    api
      .getVideos()
      .then((next) => {
        setVideos(next);
        setError(null);
      })
      .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(refresh, [refresh]);

  const refreshSettings = useCallback(() => {
    api
      .getSettings()
      .then((settings: Settings) => setIdleBackground(settings.idle_background))
      // A failure here is not worth an error banner over the library: the
      // picker simply shows "None" until the next successful read.
      .catch(() => {});
  }, []);

  useEffect(refreshSettings, [refreshSettings]);

  useSSE(EVENTS_URL, {
    onEvent: (event) => {
      // The payload is only a count of what changed, so refetch rather than
      // trying to patch the list.
      if (event.kind === "VideoLibraryChanged") {
        refresh();
      }
      // Another dashboard may have changed the fleet-wide background.
      if (event.kind === "SettingsChanged") {
        refreshSettings();
      }
    },
    onLagged: () => {
      refresh();
      refreshSettings();
    },
  });

  // A selected file can be deleted from disk and pruned by the scanner. Left
  // alone, the next Play would fail with a 404 on a video that is no longer
  // shown as selected anywhere.
  //
  // Only once a fetch has actually succeeded: an empty list because the
  // request is still in flight — or failed — is not evidence the file is gone,
  // and clearing on that would wipe the selection on every remount.
  useEffect(() => {
    if (loading || error !== null) return;
    if (selectedVideoId !== null && !videos.some((video) => video.id === selectedVideoId)) {
      onSelect(null);
    }
  }, [videos, selectedVideoId, onSelect, loading, error]);

  return (
    <section className="library" aria-label="Video library">
      <h2 className="library__title">Videos</h2>

      {loading && <p className="library__note">Loading videos…</p>}

      {error && (
        <div className="library__note library__note--error" role="alert">
          <p>Could not load videos: {error}</p>
          <button type="button" onClick={refresh}>
            Retry
          </button>
        </div>
      )}

      {!loading && !error && videos.length === 0 && (
        <p className="library__note">
          No videos found. Drop files into the server&rsquo;s videos directory and they will appear
          here.
        </p>
      )}

      {/* Resolved here rather than lifted to App: the library already owns the
          list, and App only tracks the id. */}
      {!loading && !error && videos.length > 0 && (
        <VideoPreview video={videos.find((video) => video.id === selectedVideoId) ?? null} />
      )}

      {videos.length > 0 && (
        <ul className="library__list">
          {videos.map((video) => (
            <li key={video.id}>
              {/* A native radio: single-select semantics, keyboard arrow keys
                  and screen-reader announcement all for free. */}
              <label
                className={`library__row${
                  video.id === selectedVideoId ? " library__row--selected" : ""
                }`}
              >
                <input
                  type="radio"
                  name={groupName}
                  className="library__radio"
                  checked={video.id === selectedVideoId}
                  onChange={() => onSelect(video.id)}
                />
                <Thumbnail filename={video.filename} className="thumb--row" />
                <span className="library__text">
                  <span className="library__filename" title={video.filename}>
                    {video.filename}
                  </span>
                  <span className="library__meta">
                    {/* A still has no duration, and formatDuration renders an
                        em dash — which reads as "runtime unknown" next to a
                        photo rather than "not applicable". Label the kind. */}
                    {video.kind === "Image" ? "Image" : formatDuration(video.duration_secs)} ·{" "}
                    {formatSize(video.size_bytes)}
                  </span>
                </span>
              </label>
            </li>
          ))}
        </ul>
      )}

      {/* Fleet-wide, not per TV: one setting for the whole wall. Only images
          are offered — a video would play once and stop, leaving the black
          screen this exists to avoid, and the server rejects one anyway. */}
      <div className="library__background">
        <label htmlFor={backgroundId}>Idle background</label>
        <select
          id={backgroundId}
          value={idleBackground ?? ""}
          onChange={(event) => {
            const filename = event.target.value === "" ? null : event.target.value;
            const previous = idleBackground;
            setIdleBackground(filename);
            setBackgroundError(null);
            api.setIdleBackground(filename).catch((cause: unknown) => {
              setIdleBackground(previous);
              setBackgroundError(cause instanceof Error ? cause.message : String(cause));
            });
          }}
        >
          <option value="">None (black screen)</option>
          {videos
            .filter((video) => video.kind === "Image")
            .map((video) => (
              <option key={video.id} value={video.filename}>
                {video.filename}
              </option>
            ))}
        </select>
        {backgroundError && (
          <span role="alert" className="library__background-error">
            {backgroundError}
          </span>
        )}
      </div>
    </section>
  );
}
