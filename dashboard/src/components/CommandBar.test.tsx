import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CommandBar, describeResult } from "./CommandBar";
import { api, ApiError, type PlaybackResult } from "../api";

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return {
    ...actual,
    api: {
      ...actual.api,
      play: vi.fn(),
      playAll: vi.fn(),
      pause: vi.fn(),
      resume: vi.fn(),
      stop: vi.fn(),
    },
  };
});

const mocks = {
  play: vi.mocked(api.play),
  playAll: vi.mocked(api.playAll),
  pause: vi.mocked(api.pause),
  resume: vi.mocked(api.resume),
  stop: vi.mocked(api.stop),
};

const ok = (succeeded: string[]): PlaybackResult => ({ succeeded, failed: [] });

function renderBar({
  devices = ["a"],
  video = "v1" as string | null,
}: { devices?: string[]; video?: string | null } = {}) {
  return render(<CommandBar selectedIds={new Set(devices)} selectedVideoId={video} />);
}

const button = (name: string) => screen.getByRole("button", { name: new RegExp(`^${name}$`) });

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("enabling", () => {
  it("needs both a TV and a video before Play is offered", () => {
    renderBar({ devices: [], video: null });
    expect(button("Play")).toBeDisabled();

    renderBar({ devices: ["a"], video: null });
    expect(screen.getAllByRole("button", { name: /^Play$/ })[1]).toBeDisabled();
  });

  it("enables Play once a TV and a video are chosen", () => {
    renderBar({ devices: ["a"], video: "v1" });
    expect(button("Play")).toBeEnabled();
  });

  it("offers Stop, Pause and Resume on a selection alone", () => {
    renderBar({ devices: ["a"], video: null });
    for (const label of ["Pause", "Resume", "Stop"]) {
      expect(button(label)).toBeEnabled();
    }
  });

  it("disables everything with nothing selected", () => {
    renderBar({ devices: [], video: null });
    for (const label of ["Play", "Pause", "Resume", "Stop", "Play on all"]) {
      expect(button(label)).toBeDisabled();
    }
  });

  it("offers Play on all with only a video, since the server picks the TVs", () => {
    renderBar({ devices: [], video: "v1" });
    expect(button("Play on all")).toBeEnabled();
  });
});

describe("sending commands", () => {
  it("plays the selected video on the selected TVs", async () => {
    mocks.play.mockResolvedValue(ok(["a", "b"]));
    renderBar({ devices: ["a", "b"], video: "v1" });

    act(() => button("Play").click());

    await waitFor(() =>
      expect(mocks.play).toHaveBeenCalledWith({ device_ids: ["a", "b"], video_id: "v1" }),
    );
  });

  it("routes each button to its own endpoint", async () => {
    for (const mock of [mocks.pause, mocks.resume, mocks.stop]) mock.mockResolvedValue(ok(["a"]));
    mocks.playAll.mockResolvedValue(ok(["a"]));
    renderBar({ devices: ["a"], video: "v1" });

    for (const label of ["Pause", "Resume", "Stop"]) {
      act(() => button(label).click());
      await waitFor(() => expect(screen.getByRole("status")).toBeInTheDocument());
    }
    act(() => button("Play on all").click());

    await waitFor(() => expect(mocks.playAll).toHaveBeenCalledWith("v1"));
    expect(mocks.pause).toHaveBeenCalledWith({ device_ids: ["a"] });
    expect(mocks.resume).toHaveBeenCalledWith({ device_ids: ["a"] });
    expect(mocks.stop).toHaveBeenCalledWith({ device_ids: ["a"] });
  });
});

describe("reporting the result", () => {
  it("confirms a command every TV accepted", async () => {
    mocks.play.mockResolvedValue(ok(["a", "b"]));
    renderBar({ devices: ["a", "b"], video: "v1" });

    act(() => button("Play").click());

    expect(await screen.findByRole("status")).toHaveTextContent("Playing on 2 TVs");
  });

  /// The point of the server's per-device results: "4 of 5" is knowable.
  it("says which TVs failed when only some did", async () => {
    mocks.stop.mockResolvedValue({
      succeeded: ["a"],
      failed: [{ id: "b", error: "TV-02 (10.0.0.2) did not answer POST /stop" }],
    });
    renderBar({ devices: ["a", "b"] });

    act(() => button("Stop").click());

    const toast = await screen.findByRole("alert");
    expect(toast).toHaveTextContent("Stopped on 1 of 2");
    expect(toast).toHaveTextContent("TV-02");
  });

  /// api.ts returns the body on a 502 rather than throwing, precisely so this
  /// message can name the TVs instead of saying "request failed".
  it("reports a command every TV refused as an error with the reasons", async () => {
    mocks.pause.mockResolvedValue({
      succeeded: [],
      failed: [{ id: "b", error: "TV-02 returned 500 for POST /pause: mpv socket closed" }],
    });
    renderBar({ devices: ["b"] });

    act(() => button("Pause").click());

    const toast = await screen.findByRole("alert");
    expect(toast).toHaveTextContent("Paused failed");
    expect(toast).toHaveTextContent("mpv socket closed");
  });

  it("shows the server's message when the request itself is rejected", async () => {
    mocks.playAll.mockRejectedValue(new ApiError(409, "no devices are online"));
    renderBar({ video: "v1" });

    act(() => button("Play on all").click());

    expect(await screen.findByRole("alert")).toHaveTextContent("no devices are online");
  });

  it("keeps a long failure list readable", () => {
    const result: PlaybackResult = {
      succeeded: [],
      failed: Array.from({ length: 6 }, (_, i) => ({ id: `d${i}`, error: `TV-0${i} unreachable` })),
    };

    const toast = describeResult("Playing", result);
    expect(toast.tone).toBe("error");
    expect(toast.message).toContain("(+3 more)");
  });

  it("uses the singular for one TV", () => {
    expect(describeResult("Playing", ok(["a"])).message).toBe("Playing on 1 TV");
  });
});

describe("while a command is in flight", () => {
  it("disables every button until it settles", async () => {
    let finish!: (result: PlaybackResult) => void;
    mocks.play.mockReturnValue(new Promise<PlaybackResult>((resolve) => (finish = resolve)));
    renderBar({ devices: ["a"], video: "v1" });

    act(() => button("Play").click());

    // Two commands must not race to set the same device's state.
    await waitFor(() => expect(button("Play")).toBeDisabled());
    for (const label of ["Pause", "Resume", "Stop", "Play on all"]) {
      expect(button(label)).toBeDisabled();
    }

    await act(async () => finish(ok(["a"])));
    expect(button("Play")).toBeEnabled();
  });

  it("marks the bar busy", async () => {
    let finish!: (result: PlaybackResult) => void;
    mocks.stop.mockReturnValue(new Promise<PlaybackResult>((resolve) => (finish = resolve)));
    const { container } = renderBar({ devices: ["a"] });
    const bar = container.querySelector(".commandbar")!;

    expect(bar).toHaveAttribute("aria-busy", "false");
    act(() => button("Stop").click());
    await waitFor(() => expect(bar).toHaveAttribute("aria-busy", "true"));

    await act(async () => finish(ok(["a"])));
    expect(bar).toHaveAttribute("aria-busy", "false");
  });

  it("re-enables the buttons after a failure", async () => {
    mocks.stop.mockRejectedValue(new ApiError(500, "boom"));
    renderBar({ devices: ["a"] });

    act(() => button("Stop").click());

    await screen.findByRole("alert");
    expect(button("Stop")).toBeEnabled();
  });
});

describe("the toast", () => {
  it("clears itself after a while", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      mocks.stop.mockResolvedValue(ok(["a"]));
      renderBar({ devices: ["a"] });

      act(() => button("Stop").click());
      expect(await screen.findByRole("status")).toBeInTheDocument();

      await act(async () => {
        vi.advanceTimersByTime(7000);
      });
      expect(screen.queryByRole("status")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});
