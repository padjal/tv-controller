import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, ApiError } from "./api";

/** Stand in for one fetch call. */
function respondWith(body: unknown, init: { status?: number; statusText?: string } = {}) {
  const status = init.status ?? 200;
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: init.statusText ?? "",
    json: async () => body,
  } as Response;
}

const fetchMock = vi.fn();

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("reads", () => {
  it("returns the parsed device list", async () => {
    const devices = [
      { id: "a", name: "TV-01", ip: "10.0.0.1", state: "Idle", current_video: null, last_seen: 1 },
    ];
    fetchMock.mockResolvedValue(respondWith(devices));

    await expect(api.getDevices()).resolves.toEqual(devices);
    expect(fetchMock).toHaveBeenCalledWith("/api/devices");
  });

  it("treats last_seen and size_bytes as numbers, matching the wire", async () => {
    // Regression guard: ts-rs maps i64/u64 to bigint by default, which
    // JSON.parse never produces. The shared types pin these to `number`.
    fetchMock.mockResolvedValue(
      respondWith([{ id: "v", filename: "a.mp4", duration_secs: 30, size_bytes: 8_000_000_000 }]),
    );

    const [video] = await api.getVideos();
    expect(typeof video!.size_bytes).toBe("number");
    expect(video!.size_bytes).toBe(8_000_000_000);
  });

  it("escapes ids in the path", async () => {
    fetchMock.mockResolvedValue(respondWith({}));
    await api.getVideo("a b/c");
    expect(fetchMock).toHaveBeenCalledWith("/api/videos/a%20b%2Fc");
  });

  it("throws the server's message on a 404", async () => {
    fetchMock.mockResolvedValue(respondWith({ error: "no device with id x" }, { status: 404 }));

    await expect(api.getDevice("x")).rejects.toMatchObject({
      name: "ApiError",
      status: 404,
      message: "no device with id x",
    });
  });

  it("falls back to the status when the body is not our error shape", async () => {
    fetchMock.mockResolvedValue(
      respondWith("<html>gateway</html>", { status: 503, statusText: "Service Unavailable" }),
    );

    await expect(api.getDevices()).rejects.toThrow("503 Service Unavailable");
  });
});

describe("playback commands", () => {
  it("posts the request body as JSON", async () => {
    fetchMock.mockResolvedValue(respondWith({ succeeded: ["a"], failed: [] }));

    const result = await api.play({ device_ids: ["a"], video_id: "v" });

    expect(result.succeeded).toEqual(["a"]);
    expect(fetchMock).toHaveBeenCalledWith("/api/playback/play", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ device_ids: ["a"], video_id: "v" }),
    });
  });

  it("reports partial failure without throwing", async () => {
    fetchMock.mockResolvedValue(
      respondWith({ succeeded: ["a"], failed: [{ id: "b", error: "unreachable" }] }),
    );

    const result = await api.stop({ device_ids: ["a", "b"] });
    expect(result.succeeded).toEqual(["a"]);
    expect(result.failed[0]).toEqual({ id: "b", error: "unreachable" });
  });

  /// A 502 means every device refused, but the body still says which and why —
  /// that is what a failure toast needs, so it is returned, not thrown.
  it("returns the detail on a 502 rather than throwing it away", async () => {
    fetchMock.mockResolvedValue(
      respondWith(
        { succeeded: [], failed: [{ id: "b", error: "TV-BAD returned 500" }] },
        { status: 502 },
      ),
    );

    const result = await api.pause({ device_ids: ["b"] });
    expect(result.succeeded).toEqual([]);
    expect(result.failed).toHaveLength(1);
    expect(result.failed[0]!.error).toContain("500");
  });

  it("throws on a rejected request", async () => {
    fetchMock.mockResolvedValue(respondWith({ error: "no devices are online" }, { status: 409 }));

    await expect(api.playAll("v")).rejects.toBeInstanceOf(ApiError);
    expect(fetchMock.mock.calls[0]![1]).toMatchObject({
      body: JSON.stringify({ video_id: "v" }),
    });
  });

  it("each command posts to its own endpoint", async () => {
    fetchMock.mockResolvedValue(respondWith({ succeeded: [], failed: [] }));

    await api.play({ device_ids: ["a"], video_id: "v" });
    await api.stop({ device_ids: ["a"] });
    await api.pause({ device_ids: ["a"] });
    await api.resume({ device_ids: ["a"] });
    await api.playAll("v");

    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/playback/play",
      "/api/playback/stop",
      "/api/playback/pause",
      "/api/playback/resume",
      "/api/playback/play-all",
    ]);
  });
});

describe("deleteDevice", () => {
  it("resolves on success and throws on 404", async () => {
    fetchMock.mockResolvedValue(respondWith({ id: "a", deleted: true }));
    await expect(api.deleteDevice("a")).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenCalledWith("/api/devices/a", { method: "DELETE" });

    fetchMock.mockResolvedValue(respondWith({ error: "no device with id a" }, { status: 404 }));
    await expect(api.deleteDevice("a")).rejects.toMatchObject({ status: 404 });
  });
});
