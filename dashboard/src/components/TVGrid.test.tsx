import { act, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TVGrid, formatLastSeen } from "./TVGrid";
import { api } from "../api";
import { currentEventSource, installFakeEventSource } from "../test/fakeEventSource";
import type { Device } from "../types";

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return { ...actual, api: { ...actual.api, getDevices: vi.fn() } };
});

const getDevices = vi.mocked(api.getDevices);

function device(overrides: Partial<Device> & { name: string }): Device {
  return {
    id: overrides.name,
    ip: "10.0.0.1",
    state: "Idle",
    current_video: null,
    last_seen: Math.floor(Date.now() / 1000),
    ...overrides,
  };
}

/** Render with a working selection model, as App wires it. */
function renderGrid(selected: string[] = []) {
  const onToggle = vi.fn();
  const result = render(<TVGrid selectedIds={new Set(selected)} onToggle={onToggle} />);
  return { ...result, onToggle };
}

const tiles = () => within(screen.getByRole("list", { name: "TVs" })).getAllByRole("button");

beforeEach(() => {
  installFakeEventSource();
  getDevices.mockReset();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("loading the device list", () => {
  it("renders a tile per device with its state and video", async () => {
    getDevices.mockResolvedValue([
      device({ name: "TV-01", state: "Playing", current_video: "clip.mp4" }),
    ]);
    renderGrid();

    const tile = await screen.findByRole("button", { name: /TV-01/ });
    expect(tile).toHaveTextContent("Playing");
    expect(tile).toHaveTextContent("clip.mp4");
  });

  it("shows an empty state rather than a bare grid", async () => {
    getDevices.mockResolvedValue([]);
    renderGrid();

    expect(await screen.findByText(/No TVs registered yet/)).toBeTruthy();
  });

  it("surfaces a failure with a retry", async () => {
    getDevices.mockRejectedValueOnce(new Error("500 Internal Server Error"));
    renderGrid();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("500 Internal Server Error");

    getDevices.mockResolvedValue([device({ name: "TV-01" })]);
    act(() => screen.getByRole("button", { name: "Retry" }).click());

    expect(await screen.findByRole("button", { name: /TV-01/ })).toBeTruthy();
  });

  it("orders devices by name, as the server does", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-02" }), device({ name: "TV-01" })]);
    renderGrid();

    await screen.findByRole("button", { name: /TV-01/ });
    expect(tiles().map((tile) => tile.textContent)).toEqual([
      expect.stringContaining("TV-01"),
      expect.stringContaining("TV-02"),
    ]);
  });
});

describe("live updates", () => {
  it("merges a DeviceUpdated event into the matching tile", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-01", state: "Idle" })]);
    renderGrid();
    await screen.findByRole("button", { name: /TV-01/ });

    act(() =>
      currentEventSource().emit({
        kind: "DeviceUpdated",
        payload: device({ name: "TV-01", state: "Playing", current_video: "clip.mp4" }),
      }),
    );

    const tile = screen.getByRole("button", { name: /TV-01/ });
    expect(tile).toHaveTextContent("Playing");
    expect(tile).toHaveTextContent("clip.mp4");
    expect(tiles()).toHaveLength(1);
  });

  it("adds a TV that registers while the page is open", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-01" })]);
    renderGrid();
    await screen.findByRole("button", { name: /TV-01/ });

    act(() =>
      currentEventSource().emit({ kind: "DeviceUpdated", payload: device({ name: "TV-02" }) }),
    );

    expect(tiles()).toHaveLength(2);
  });

  it("applies every event in a burst, not just the last", async () => {
    // Playing on several TVs emits one event each, within milliseconds.
    getDevices.mockResolvedValue([device({ name: "TV-01" }), device({ name: "TV-02" })]);
    renderGrid();
    await screen.findByRole("button", { name: /TV-01/ });

    act(() => {
      const source = currentEventSource();
      source.emit({
        kind: "DeviceUpdated",
        payload: device({ name: "TV-01", state: "Playing", current_video: "a.mp4" }),
      });
      source.emit({
        kind: "DeviceUpdated",
        payload: device({ name: "TV-02", state: "Playing", current_video: "a.mp4" }),
      });
    });

    for (const name of ["TV-01", "TV-02"]) {
      expect(screen.getByRole("button", { name: new RegExp(name) })).toHaveTextContent("Playing");
    }
  });

  it("shows a device going offline", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-01", state: "Playing" })]);
    renderGrid();
    await screen.findByRole("button", { name: /TV-01/ });

    act(() =>
      currentEventSource().emit({
        kind: "DeviceOffline",
        payload: device({
          name: "TV-01",
          state: "Offline",
          current_video: null,
          last_seen: Math.floor(Date.now() / 1000) - 120,
        }),
      }),
    );

    const tile = screen.getByRole("button", { name: /TV-01/ });
    expect(tile).toHaveTextContent("Offline");
    expect(tile).toHaveTextContent("last seen 2m ago");
  });

  it("ignores video library events", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-01" })]);
    renderGrid();
    await screen.findByRole("button", { name: /TV-01/ });
    getDevices.mockClear();

    act(() =>
      currentEventSource().emit({ kind: "VideoLibraryChanged", payload: { upserted: 1 } }),
    );

    expect(getDevices).not.toHaveBeenCalled();
    expect(tiles()).toHaveLength(1);
  });

  it("refetches when the server says events were dropped", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-01" })]);
    renderGrid();
    await screen.findByRole("button", { name: /TV-01/ });

    getDevices.mockResolvedValue([device({ name: "TV-01" }), device({ name: "TV-02" })]);
    act(() => currentEventSource().emitNamed("lagged", "40"));

    await waitFor(() => expect(tiles()).toHaveLength(2));
  });

  it("opens one connection and closes it on unmount", async () => {
    getDevices.mockResolvedValue([]);
    const { unmount } = renderGrid();
    await screen.findByText(/No TVs registered yet/);

    const source = currentEventSource();
    expect(source.url).toBe("/api/events");

    unmount();
    expect(source.closed).toBe(true);
  });
});

describe("selection", () => {
  it("reports a click to the parent", async () => {
    getDevices.mockResolvedValue([device({ name: "TV-01", id: "abc" })]);
    const { onToggle } = renderGrid();

    (await screen.findByRole("button", { name: /TV-01/ })).click();

    expect(onToggle).toHaveBeenCalledWith("abc");
  });

  it("marks selected tiles as pressed", async () => {
    getDevices.mockResolvedValue([
      device({ name: "TV-01", id: "a" }),
      device({ name: "TV-02", id: "b" }),
    ]);
    renderGrid(["a"]);

    await screen.findByRole("button", { name: /TV-01/ });
    expect(screen.getByRole("button", { name: /TV-01/ }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: /TV-02/ }).getAttribute("aria-pressed")).toBe("false");
  });
});

describe("formatLastSeen", () => {
  const now = 1_000_000_000_000; // ms

  it("scales the unit with the gap", () => {
    const at = (secondsAgo: number) => formatLastSeen(now / 1000 - secondsAgo, now);
    expect(at(5)).toBe("5s ago");
    expect(at(90)).toBe("1m ago");
    expect(at(3 * 3600 + 60)).toBe("3h ago");
    expect(at(50 * 3600)).toBe("2d ago");
  });

  it("never reports a negative age when clocks disagree", () => {
    // The Pi and the server need not agree to the second.
    expect(formatLastSeen(now / 1000 + 30, now)).toBe("0s ago");
  });
});
