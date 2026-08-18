import { useCallback, useEffect, useState } from "react";
import { api, type PlaybackResult } from "../api";
import "./CommandBar.css";

export interface CommandBarProps {
  selectedIds: Set<string>;
  selectedVideoId: string | null;
}

type Tone = "success" | "partial" | "error";

interface Toast {
  tone: Tone;
  message: string;
  /** Changes on every command so repeating one re-announces it. */
  key: number;
}

/** How long a result stays on screen before fading out. */
const TOAST_MS = 6000;

/** Failure lines are long (they name the device and quote the agent). */
const MAX_LISTED_FAILURES = 3;

/**
 * Turn a per-device result into something worth reading.
 *
 * The server reports each device separately, so "played on 4 of 5" is
 * knowable — and much more useful than a bare success or failure.
 */
export function describeResult(verb: string, result: PlaybackResult): Toast {
  const ok = result.succeeded.length;
  const failed = result.failed.length;
  const key = Date.now();

  if (failed === 0) {
    return { tone: "success", message: `${verb} on ${ok} ${ok === 1 ? "TV" : "TVs"}`, key };
  }

  const reasons = result.failed
    .slice(0, MAX_LISTED_FAILURES)
    .map((failure) => failure.error)
    .join("; ");
  const more = failed > MAX_LISTED_FAILURES ? ` (+${failed - MAX_LISTED_FAILURES} more)` : "";

  if (ok === 0) {
    return { tone: "error", message: `${verb} failed: ${reasons}${more}`, key };
  }
  return {
    tone: "partial",
    message: `${verb} on ${ok} of ${ok + failed}; ${reasons}${more}`,
    key,
  };
}

export function CommandBar({ selectedIds, selectedVideoId }: CommandBarProps) {
  const [pending, setPending] = useState<string | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);

  const deviceIds = Array.from(selectedIds);
  const hasDevices = deviceIds.length > 0;
  const hasVideo = selectedVideoId !== null;
  const busy = pending !== null;

  // Auto-dismiss, re-armed whenever a new result arrives.
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), TOAST_MS);
    return () => clearTimeout(timer);
  }, [toast]);

  const run = useCallback(async (label: string, verb: string, call: () => Promise<PlaybackResult>) => {
    setPending(label);
    setToast(null);
    try {
      setToast(describeResult(verb, await call()));
    } catch (cause: unknown) {
      // A rejected request: unknown video, nothing online, bad input.
      setToast({
        tone: "error",
        message: cause instanceof Error ? cause.message : String(cause),
        key: Date.now(),
      });
    } finally {
      setPending(null);
    }
  }, []);

  const commands = [
    {
      label: "Play",
      enabled: hasDevices && hasVideo,
      run: () =>
        run("Play", "Playing", () =>
          // `hasVideo` guards this; the non-null assertion is the price of
          // narrowing across a closure.
          api.play({ device_ids: deviceIds, video_id: selectedVideoId! }),
        ),
    },
    {
      label: "Pause",
      enabled: hasDevices,
      run: () => run("Pause", "Paused", () => api.pause({ device_ids: deviceIds })),
    },
    {
      label: "Resume",
      enabled: hasDevices,
      run: () => run("Resume", "Resumed", () => api.resume({ device_ids: deviceIds })),
    },
    {
      label: "Stop",
      enabled: hasDevices,
      run: () => run("Stop", "Stopped", () => api.stop({ device_ids: deviceIds })),
    },
    {
      // Not in the task's button list, but the endpoint exists and is
      // otherwise unreachable from the UI. Needs no device selection: the
      // server targets everything that is not offline.
      label: "Play on all",
      enabled: hasVideo,
      run: () => run("Play on all", "Playing", () => api.playAll(selectedVideoId!)),
    },
  ];

  return (
    <div className="commandbar" aria-busy={busy}>
      <div className="commandbar__inner">
        <p className="commandbar__summary">
          {hasDevices ? `${deviceIds.length} selected` : "No TVs selected"}
          {hasVideo ? "" : " · no video chosen"}
        </p>

        <div className="commandbar__buttons">
          {commands.map((command) => (
            <button
              key={command.label}
              type="button"
              className="commandbar__button"
              // Everything is disabled while a command is in flight, so two
              // commands cannot race to set a device's state.
              disabled={!command.enabled || busy}
              onClick={() => void command.run()}
            >
              {pending === command.label && <span className="commandbar__spinner" aria-hidden />}
              {command.label}
            </button>
          ))}
        </div>
      </div>

      {toast && (
        <p
          key={toast.key}
          className={`commandbar__toast commandbar__toast--${toast.tone}`}
          role={toast.tone === "success" ? "status" : "alert"}
        >
          {toast.message}
        </p>
      )}
    </div>
  );
}
