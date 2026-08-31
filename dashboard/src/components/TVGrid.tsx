import { useCallback, useEffect, useState } from "react";
import { api, EVENTS_URL } from "../api";
import { useSSE } from "../hooks/useSSE";
import type { Device, DeviceState } from "../types";
import { Thumbnail } from "./Thumbnail";
import "./TVGrid.css";

export interface TVGridProps {
  /** Selection lives in the parent, so CommandBar can act on it. */
  selectedIds: Set<string>;
  onToggle: (id: string) => void;
}

/** The server orders devices by name; keep merges in the same order. */
function byName(devices: Device[]): Device[] {
  return [...devices].sort((a, b) => a.name.localeCompare(b.name));
}

/** Insert or replace a device, keeping the list sorted. */
function merge(devices: Device[], updated: Device): Device[] {
  const known = devices.some((device) => device.id === updated.id);
  return byName(
    known ? devices.map((device) => (device.id === updated.id ? updated : device)) : [...devices, updated],
  );
}

const ROTATIONS = [0, 90, 180, 270] as const;

/**
 * Per-TV rotation, for screens that are physically mounted turned.
 *
 * Optimistic: the select shows the new angle immediately and rolls back if the
 * server rejects it. The authoritative value arrives moments later as a
 * DeviceUpdated event, which is what keeps other open dashboards in step.
 */
function RotationControl({ device }: { device: Device }) {
  const [pending, setPending] = useState<number | null>(null);
  const [failed, setFailed] = useState(false);

  // A newer value from the server supersedes whatever we were showing.
  useEffect(() => {
    setPending(null);
  }, [device.rotation]);

  const value = pending ?? device.rotation;

  async function change(rotation: number) {
    setPending(rotation);
    setFailed(false);
    try {
      await api.setRotation(device.id, rotation);
    } catch {
      // The TV is unreachable or the value was refused; show what the server
      // still holds rather than a rotation that never took.
      setPending(null);
      setFailed(true);
    }
  }

  const id = `rotation-${device.id}`;
  return (
    <span className="tv-tile__rotation">
      <label htmlFor={id}>Rotation</label>
      <select
        id={id}
        value={value}
        onChange={(event) => void change(Number(event.target.value))}
      >
        {ROTATIONS.map((degrees) => (
          <option key={degrees} value={degrees}>
            {degrees}°
          </option>
        ))}
      </select>
      {failed && (
        <span role="alert" className="tv-tile__rotation-error">
          failed
        </span>
      )}
    </span>
  );
}

const STATE_LABEL: Record<DeviceState, string> = {
  Idle: "Idle",
  Playing: "Playing",
  Paused: "Paused",
  Offline: "Offline",
};

/**
 * How long ago a device was last heard from.
 *
 * Only shown for offline tiles: the heartbeat deliberately leaves `last_seen`
 * at its old value when it marks a device Offline, so this reads as "gone for
 * this long" rather than "checked this recently".
 */
export function formatLastSeen(lastSeen: number, now: number = Date.now()): string {
  const seconds = Math.max(0, Math.floor(now / 1000 - lastSeen));
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86_400)}d ago`;
}

export function TVGrid({ selectedIds, onToggle }: TVGridProps) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(() => {
    api
      .getDevices()
      .then((next) => {
        setDevices(byName(next));
        setError(null);
      })
      .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(refresh, [refresh]);

  useSSE(EVENTS_URL, {
    onEvent: (event) => {
      // Both kinds carry a whole Device, so both merge the same way — the
      // difference is only which subsystem sent it.
      if (event.kind === "DeviceUpdated" || event.kind === "DeviceOffline") {
        setDevices((current) => merge(current, event.payload as Device));
      }
    },
    // Events were dropped, so the local list can no longer be trusted.
    onLagged: refresh,
  });

  if (loading) {
    return <p className="tv-grid__note">Loading TVs…</p>;
  }

  if (error) {
    return (
      <div className="tv-grid__note tv-grid__note--error" role="alert">
        <p>Could not load TVs: {error}</p>
        <button type="button" onClick={refresh}>
          Retry
        </button>
      </div>
    );
  }

  if (devices.length === 0) {
    return (
      <p className="tv-grid__note">
        No TVs registered yet. Start a pi-agent and it will appear here.
      </p>
    );
  }

  return (
    <ul className="tv-grid" aria-label="TVs">
      {devices.map((device) => {
        const selected = selectedIds.has(device.id);
        return (
          <li
            key={device.id}
            className={`tv-tile${selected ? " tv-tile--selected" : ""}`}
          >
            {/* The selectable region is a button rather than the whole tile:
                selection has to work from the keyboard and be announced, but a
                <select> cannot live inside a <button> — nested interactive
                elements are invalid, and every click on the dropdown would
                bubble into the toggle. So the tile is the container and the
                rotation control is the button's sibling. */}
            <button
              type="button"
              className="tv-tile__select"
              aria-pressed={selected}
              onClick={() => onToggle(device.id)}
            >
              {/* Offline tiles show when the TV was last seen rather than a
                  poster — whatever it was playing is no longer on screen. */}
              <Thumbnail
                filename={device.state === "Offline" ? null : device.current_video}
                className="thumb--tile"
              />
              <span className="tv-tile__name">{device.name}</span>
              <span className={`tv-tile__badge tv-tile__badge--${device.state.toLowerCase()}`}>
                {STATE_LABEL[device.state]}
              </span>
              <span className="tv-tile__video">
                {device.state === "Offline"
                  ? `last seen ${formatLastSeen(device.last_seen)}`
                  : (device.current_video ?? "—")}
              </span>
            </button>
            <RotationControl device={device} />
          </li>
        );
      })}
    </ul>
  );
}
