import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { VideoLibrary, formatDuration, formatSize } from "./VideoLibrary";
import { api } from "../api";
import { currentEventSource, installFakeEventSource } from "../test/fakeEventSource";
import type { Video } from "../types";

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return { ...actual, api: { ...actual.api, getVideos: vi.fn() } };
});

const getVideos = vi.mocked(api.getVideos);

function video(overrides: Partial<Video> & { filename: string }): Video {
  return {
    id: overrides.filename,
    duration_secs: 90,
    size_bytes: 1024 * 1024,
    ...overrides,
  };
}

function renderLibrary(selectedVideoId: string | null = null) {
  const onSelect = vi.fn();
  const result = render(
    <VideoLibrary selectedVideoId={selectedVideoId} onSelect={onSelect} />,
  );
  return { ...result, onSelect };
}

beforeEach(() => {
  installFakeEventSource();
  getVideos.mockReset();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("loading the library", () => {
  it("lists each video with its duration and size", async () => {
    getVideos.mockResolvedValue([
      video({ filename: "clip.mp4", duration_secs: 95, size_bytes: 5 * 1024 * 1024 }),
    ]);
    renderLibrary();

    const row = await screen.findByRole("radio", { name: /clip\.mp4/ });
    expect(row.closest("label")).toHaveTextContent("1:35");
    expect(row.closest("label")).toHaveTextContent("5.0 MB");
  });

  it("shows an empty state pointing at the videos directory", async () => {
    getVideos.mockResolvedValue([]);
    renderLibrary();

    expect(await screen.findByText(/No videos found/)).toBeTruthy();
  });

  it("surfaces a failure with a retry", async () => {
    getVideos.mockRejectedValueOnce(new Error("503 Service Unavailable"));
    renderLibrary();

    expect(await screen.findByRole("alert")).toHaveTextContent("503 Service Unavailable");

    getVideos.mockResolvedValue([video({ filename: "clip.mp4" })]);
    act(() => screen.getByRole("button", { name: "Retry" }).click());

    expect(await screen.findByRole("radio", { name: /clip\.mp4/ })).toBeTruthy();
  });

  it("renders a video with no duration rather than hiding it", async () => {
    // ffprobe is optional on the server; duration is null when it is missing.
    getVideos.mockResolvedValue([video({ filename: "clip.mkv", duration_secs: null })]);
    renderLibrary();

    const row = await screen.findByRole("radio", { name: /clip\.mkv/ });
    expect(row.closest("label")).toHaveTextContent("—");
  });
});

describe("selection", () => {
  it("reports the chosen video to the parent", async () => {
    getVideos.mockResolvedValue([video({ filename: "clip.mp4", id: "v1" })]);
    const { onSelect } = renderLibrary();

    (await screen.findByRole("radio", { name: /clip\.mp4/ })).click();

    expect(onSelect).toHaveBeenCalledWith("v1");
  });

  it("marks only the selected row as checked", async () => {
    getVideos.mockResolvedValue([
      video({ filename: "a.mp4", id: "a" }),
      video({ filename: "b.mp4", id: "b" }),
    ]);
    renderLibrary("b");

    await screen.findByRole("radio", { name: /a\.mp4/ });
    expect(screen.getByRole("radio", { name: /a\.mp4/ })).not.toBeChecked();
    expect(screen.getByRole("radio", { name: /b\.mp4/ })).toBeChecked();
  });

  /// A file can be deleted from disk while it is selected; the scanner prunes
  /// the row, and a Play on a stale id would 404.
  it("clears a selection whose file has disappeared", async () => {
    getVideos.mockResolvedValue([video({ filename: "gone.mp4", id: "gone" })]);
    const { onSelect, rerender } = renderLibrary("gone");
    await screen.findByRole("radio", { name: /gone\.mp4/ });
    expect(onSelect).not.toHaveBeenCalled();

    getVideos.mockResolvedValue([]);
    act(() =>
      currentEventSource().emit({ kind: "VideoLibraryChanged", payload: { pruned: 1 } }),
    );

    await waitFor(() => expect(onSelect).toHaveBeenCalledWith(null));
    rerender(<VideoLibrary selectedVideoId={null} onSelect={onSelect} />);
  });

  it("leaves a still-present selection alone", async () => {
    getVideos.mockResolvedValue([video({ filename: "keep.mp4", id: "keep" })]);
    const { onSelect } = renderLibrary("keep");
    await screen.findByRole("radio", { name: /keep\.mp4/ });

    act(() =>
      currentEventSource().emit({ kind: "VideoLibraryChanged", payload: { upserted: 1 } }),
    );

    await waitFor(() => expect(getVideos).toHaveBeenCalledTimes(2));
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe("live updates", () => {
  it("refetches when the scanner reports a change", async () => {
    getVideos.mockResolvedValue([video({ filename: "a.mp4", id: "a" })]);
    renderLibrary();
    await screen.findByRole("radio", { name: /a\.mp4/ });

    getVideos.mockResolvedValue([
      video({ filename: "a.mp4", id: "a" }),
      video({ filename: "b.mp4", id: "b" }),
    ]);
    act(() =>
      currentEventSource().emit({ kind: "VideoLibraryChanged", payload: { upserted: 1 } }),
    );

    expect(await screen.findByRole("radio", { name: /b\.mp4/ })).toBeTruthy();
  });

  it("ignores device events", async () => {
    getVideos.mockResolvedValue([video({ filename: "a.mp4" })]);
    renderLibrary();
    await screen.findByRole("radio", { name: /a\.mp4/ });
    getVideos.mockClear();

    act(() => currentEventSource().emit({ kind: "DeviceUpdated", payload: { name: "TV-01" } }));

    expect(getVideos).not.toHaveBeenCalled();
  });

  it("refetches when the server says events were dropped", async () => {
    getVideos.mockResolvedValue([video({ filename: "a.mp4" })]);
    renderLibrary();
    await screen.findByRole("radio", { name: /a\.mp4/ });
    getVideos.mockClear();

    act(() => currentEventSource().emitNamed("lagged", "80"));

    await waitFor(() => expect(getVideos).toHaveBeenCalledTimes(1));
  });
});

describe("formatDuration", () => {
  it("formats below and above an hour", () => {
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(5)).toBe("0:05");
    expect(formatDuration(95)).toBe("1:35");
    expect(formatDuration(3600)).toBe("1:00:00");
    expect(formatDuration(3725)).toBe("1:02:05");
  });

  it("renders a dash when there is no duration", () => {
    expect(formatDuration(null)).toBe("—");
    expect(formatDuration(-1)).toBe("—");
  });
});

describe("formatSize", () => {
  it("scales the unit", () => {
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(2048)).toBe("2.0 KB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatSize(3.5 * 1024 ** 3)).toBe("3.5 GB");
  });

  it("handles a video larger than 32 bits", () => {
    // size_bytes is u64 in Rust; the wire value is a plain JSON number.
    expect(formatSize(8_000_000_000)).toBe("7.5 GB");
  });
});

describe("selection is not cleared spuriously", () => {
  /// Regression: the reconciliation effect used to run before the first fetch
  /// resolved, when the list is still empty, and wiped a valid selection.
  it("keeps the selection while the list is still loading", async () => {
    let resolve!: (videos: Video[]) => void;
    getVideos.mockReturnValue(new Promise<Video[]>((r) => (resolve = r)));

    const { onSelect } = renderLibrary("pending");
    expect(await screen.findByText(/Loading videos/)).toBeTruthy();
    expect(onSelect).not.toHaveBeenCalled();

    await act(async () => {
      resolve([video({ filename: "pending.mp4", id: "pending" })]);
    });
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("keeps the selection when the list could not be loaded", async () => {
    getVideos.mockRejectedValue(new Error("network down"));
    const { onSelect } = renderLibrary("keep");

    await screen.findByRole("alert");
    expect(onSelect).not.toHaveBeenCalled();
  });
});
