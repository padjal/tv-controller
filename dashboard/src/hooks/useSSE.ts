import { useEffect, useRef, useState } from "react";
import type { SseEvent } from "../types";

export type SseStatus = "connecting" | "open" | "closed";

export interface SseHandlers {
  /** Called once per event, in arrival order. */
  onEvent: (event: SseEvent) => void;
  /**
   * The server dropped events because this client fell behind (more than 64
   * queued). Whatever was missed is gone, so the right response is to refetch
   * rather than to keep merging deltas.
   */
  onLagged?: (dropped: number) => void;
}

/**
 * Subscribe to the server's event stream.
 *
 * Deliberately callback-based rather than returning the last event as state.
 * A `useState`-per-event hook loses events: playing on five TVs broadcasts five
 * `DeviceUpdated` events within milliseconds, React coalesces the renders, and
 * a consumer watching `lastEvent` only ever sees the fifth — the other four
 * tiles would silently stay stale.
 *
 * @returns the current connection status, for a header indicator.
 */
export function useSSE(url: string, handlers: SseHandlers): SseStatus {
  const [status, setStatus] = useState<SseStatus>("connecting");

  // Held in a ref so that a caller passing an inline arrow function does not
  // tear down and re-open the connection on every render.
  const handlersRef = useRef(handlers);
  useEffect(() => {
    handlersRef.current = handlers;
  });

  useEffect(() => {
    const source = new EventSource(url);
    setStatus("connecting");

    source.onopen = () => setStatus("open");

    source.onmessage = (message: MessageEvent<string>) => {
      let event: SseEvent;
      try {
        event = JSON.parse(message.data) as SseEvent;
      } catch {
        // A malformed frame must not take down the dashboard.
        console.warn("ignoring unparseable SSE frame", message.data);
        return;
      }
      handlersRef.current.onEvent(event);
    };

    // The server sends this as a named event when a subscriber lags; named
    // events do not fire `onmessage`, hence the explicit listener.
    source.addEventListener("lagged", (message) => {
      const dropped = Number((message as MessageEvent<string>).data) || 0;
      console.warn(`missed ${dropped} events; resyncing`);
      handlersRef.current.onLagged?.(dropped);
    });

    // EventSource reconnects on its own; an error means "not connected right
    // now", not "give up".
    source.onerror = () => {
      setStatus(source.readyState === EventSource.CLOSED ? "closed" : "connecting");
    };

    return () => {
      source.close();
      setStatus("closed");
    };
  }, [url]);

  return status;
}
