import { useCallback, useEffect, useState } from "react";
import { api, EVENTS_URL } from "./api";
import { useSSE } from "./hooks/useSSE";
import type { Device, Video } from "./types";

/**
 * Placeholder shell. TVGrid, VideoLibrary and CommandBar replace this in
 * tasks 4.3-4.5; for now it proves the API client and event stream are wired.
 */
export function App() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [videos, setVideos] = useState<Video[]>([]);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    Promise.all([api.getDevices(), api.getVideos()])
      .then(([nextDevices, nextVideos]) => {
        setDevices(nextDevices);
        setVideos(nextVideos);
        setError(null);
      })
      .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)));
  }, []);

  useEffect(refresh, [refresh]);

  const status = useSSE(EVENTS_URL, {
    onEvent: (event) => {
      if (event.kind === "DeviceUpdated" || event.kind === "DeviceOffline") {
        const updated = event.payload as Device;
        setDevices((current) =>
          current.some((device) => device.id === updated.id)
            ? current.map((device) => (device.id === updated.id ? updated : device))
            : [...current, updated],
        );
      } else {
        refresh();
      }
    },
    // Events were dropped, so deltas can no longer be trusted.
    onLagged: refresh,
  });

  return (
    <main>
      <h1>TV Controller</h1>
      <p>stream: {status}</p>
      {error && <p role="alert">{error}</p>}
      <p>
        {devices.length} device(s), {videos.length} video(s)
      </p>
      <ul>
        {devices.map((device) => (
          <li key={device.id}>
            {device.name} — {device.state}
            {device.current_video ? ` — ${device.current_video}` : ""}
          </li>
        ))}
      </ul>
    </main>
  );
}
