import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useSSE } from "./useSSE";
import {
  currentEventSource,
  FakeEventSource,
  installFakeEventSource,
} from "../test/fakeEventSource";
import type { SseEvent } from "../types";

function anEvent(name: string): SseEvent {
  return {
    kind: "DeviceUpdated",
    payload: { name },
  };
}

const latest = currentEventSource;

beforeEach(() => {
  installFakeEventSource();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("useSSE", () => {
  it("connects to the given url and closes on unmount", () => {
    const { unmount } = renderHook(() => useSSE("/api/events", { onEvent: vi.fn() }));

    expect(latest().url).toBe("/api/events");
    expect(latest().closed).toBe(false);

    unmount();
    expect(latest().closed).toBe(true);
  });

  it("delivers every event, not just the most recent", () => {
    // The reason this hook is callback-based: playing on several TVs emits a
    // burst of DeviceUpdated events, and a useState-per-event hook would
    // coalesce them into one render and drop all but the last.
    const seen: string[] = [];
    renderHook(() =>
      useSSE("/api/events", {
        onEvent: (event) => seen.push((event.payload as { name: string }).name),
      }),
    );

    act(() => {
      latest().emit(anEvent("TV-01"));
      latest().emit(anEvent("TV-02"));
      latest().emit(anEvent("TV-03"));
    });

    expect(seen).toEqual(["TV-01", "TV-02", "TV-03"]);
  });

  it("passes the parsed event through", () => {
    const onEvent = vi.fn();
    renderHook(() => useSSE("/api/events", { onEvent }));

    act(() => latest().emit({ kind: "VideoLibraryChanged", payload: { upserted: 2 } }));

    expect(onEvent).toHaveBeenCalledWith({
      kind: "VideoLibraryChanged",
      payload: { upserted: 2 },
    });
  });

  it("ignores an unparseable frame instead of crashing", () => {
    const onEvent = vi.fn();
    renderHook(() => useSSE("/api/events", { onEvent }));

    act(() => latest().emitRaw("not json"));
    expect(onEvent).not.toHaveBeenCalled();

    // And keeps working afterwards.
    act(() => latest().emit(anEvent("TV-01")));
    expect(onEvent).toHaveBeenCalledTimes(1);
  });

  it("reports the server's lagged event so the caller can resync", () => {
    const onLagged = vi.fn();
    renderHook(() => useSSE("/api/events", { onEvent: vi.fn(), onLagged }));

    act(() => latest().emitNamed("lagged", "17"));

    expect(onLagged).toHaveBeenCalledWith(17);
  });

  it("does not reconnect when the caller passes a fresh closure each render", () => {
    // The common call site is an inline arrow function; re-subscribing on every
    // render would drop events and hammer the server.
    const { rerender } = renderHook(() => useSSE("/api/events", { onEvent: () => {} }));

    rerender();
    rerender();

    expect(FakeEventSource.instances).toHaveLength(1);
  });

  it("always calls the newest callback", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ handler }) => useSSE("/api/events", { onEvent: handler }), {
      initialProps: { handler: first },
    });

    rerender({ handler: second });
    act(() => latest().emit(anEvent("TV-01")));

    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("reconnects when the url changes", () => {
    const { rerender } = renderHook(({ url }) => useSSE(url, { onEvent: vi.fn() }), {
      initialProps: { url: "/api/events" },
    });

    rerender({ url: "/other" });

    expect(FakeEventSource.instances).toHaveLength(2);
    expect(FakeEventSource.instances[0]!.closed).toBe(true);
    expect(latest().url).toBe("/other");
  });

  it("tracks connection status", () => {
    const { result } = renderHook(() => useSSE("/api/events", { onEvent: vi.fn() }));
    expect(result.current).toBe("connecting");

    act(() => latest().open());
    expect(result.current).toBe("open");

    // A dropped connection is "connecting": EventSource retries on its own.
    act(() => latest().fail());
    expect(result.current).toBe("connecting");

    act(() => latest().fail({ closed: true }));
    expect(result.current).toBe("closed");
  });
});
