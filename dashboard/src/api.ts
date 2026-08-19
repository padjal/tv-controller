import type { Device, PlaybackRequest, StopRequest, Video } from "./types";

/** Same origin: the Rust server serves this bundle and the API. */
const BASE = "";

export const EVENTS_URL = `${BASE}/api/events`;

/**
 * Where the server serves a video file itself, as opposed to its metadata.
 *
 * `tower_http`'s ServeDir is mounted at `/videos` and already answers Range
 * requests, which is what lets a browser seek in a preview without pulling the
 * whole file. The filename is encoded because the library is full of spaces
 * and `#`, either of which would otherwise truncate or fragment the URL.
 */
export function videoFileUrl(filename: string): string {
  return `${BASE}/videos/${encodeURIComponent(filename)}`;
}

/**
 * The generated poster frame for a video, or `null` when there is no filename
 * to build one from.
 *
 * Keyed on the filename rather than the video id, because `Device` carries
 * `current_video` as a filename and has no id — so a tile can build this from
 * what it already holds instead of resolving the library first.
 *
 * The server 404s when ffmpeg was unavailable or could not decode the file, so
 * callers must handle the image failing to load.
 */
export function thumbnailUrl(filename: string): string {
  return `${BASE}/thumbnails/${encodeURIComponent(filename)}.jpg`;
}

/**
 * A request the server rejected. `status` lets a caller tell apart the cases
 * the server distinguishes: 404 unknown id, 409 nothing online, 400 bad input.
 */
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/**
 * Per-device outcome of a playback command.
 *
 * Mirrors `PlaybackResponse` in `server/src/handlers/playback.rs`. That type is
 * server-local rather than in `shared`, so this is hand-written and must be
 * kept in step with it.
 */
export interface PlaybackResult {
  succeeded: string[];
  failed: { id: string; error: string }[];
}

/** The server's error body: `{"error": "..."}` (see server/src/error.rs). */
async function errorMessage(response: Response): Promise<string> {
  try {
    const body: unknown = await response.json();
    if (
      typeof body === "object" &&
      body !== null &&
      "error" in body &&
      typeof (body as { error: unknown }).error === "string"
    ) {
      return (body as { error: string }).error;
    }
  } catch {
    // Fall through to the status text — an error page from a proxy, say.
  }
  return `${response.status} ${response.statusText}`;
}

async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(`${BASE}${path}`);
  if (!response.ok) {
    throw new ApiError(response.status, await errorMessage(response));
  }
  return (await response.json()) as T;
}

/**
 * POST a playback command.
 *
 * A 502 means every targeted device refused, but the body is still a
 * `PlaybackResult` listing why each one failed — that detail is exactly what a
 * failure toast should show, so it is returned rather than thrown. Genuine
 * request errors (400, 404, 409, 500) throw.
 */
async function postCommand(path: string, body: unknown): Promise<PlaybackResult> {
  const response = await fetch(`${BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (response.ok || response.status === 502) {
    return (await response.json()) as PlaybackResult;
  }
  throw new ApiError(response.status, await errorMessage(response));
}

export const api = {
  getDevices: (): Promise<Device[]> => getJson<Device[]>("/api/devices"),

  getDevice: (id: string): Promise<Device> =>
    getJson<Device>(`/api/devices/${encodeURIComponent(id)}`),

  deleteDevice: async (id: string): Promise<void> => {
    const response = await fetch(`${BASE}/api/devices/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
    if (!response.ok) {
      throw new ApiError(response.status, await errorMessage(response));
    }
  },

  getVideos: (): Promise<Video[]> => getJson<Video[]>("/api/videos"),

  getVideo: (id: string): Promise<Video> =>
    getJson<Video>(`/api/videos/${encodeURIComponent(id)}`),

  play: (body: PlaybackRequest): Promise<PlaybackResult> =>
    postCommand("/api/playback/play", body),

  playAll: (videoId: string): Promise<PlaybackResult> =>
    postCommand("/api/playback/play-all", { video_id: videoId }),

  stop: (body: StopRequest): Promise<PlaybackResult> => postCommand("/api/playback/stop", body),

  pause: (body: StopRequest): Promise<PlaybackResult> => postCommand("/api/playback/pause", body),

  resume: (body: StopRequest): Promise<PlaybackResult> => postCommand("/api/playback/resume", body),
};
