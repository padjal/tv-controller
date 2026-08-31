import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { VideoPreview } from "./VideoPreview";
import type { Video } from "../types";

function video(overrides: Partial<Video> & { filename: string }): Video {
  return {
    id: overrides.filename,
    duration_secs: 30,
    kind: "Video",
    size_bytes: 1024 * 1024,
    ...overrides,
  };
}

describe("VideoPreview", () => {
  it("prompts for a selection when nothing is chosen", () => {
    render(<VideoPreview video={null} />);
    expect(screen.getByTestId("preview-empty")).toBeInTheDocument();
  });

  it("points the player at the file the server serves", () => {
    render(<VideoPreview video={video({ filename: "promo.mp4" })} />);

    const player = screen.getByLabelText("Preview of promo.mp4");
    expect(player).toHaveAttribute("src", "/videos/promo.mp4");
    expect(player).toHaveAttribute("controls");
  });

  it("encodes names with spaces and hashes", () => {
    // The real library is full of these — "golden times.mp4", "summer #2.mp4".
    // Unencoded, a space breaks the URL and a "#" truncates it into a
    // fragment, so the request never reaches the file.
    render(<VideoPreview video={video({ filename: "golden times #2.mp4" })} />);

    expect(screen.getByLabelText("Preview of golden times #2.mp4")).toHaveAttribute(
      "src",
      "/videos/golden%20times%20%232.mp4",
    );
  });

  it("only fetches metadata until the operator presses play", () => {
    // Several dashboards may be open at once. Preloading whole files would put
    // a multi-megabit stream on the LAN for a video nobody asked to watch,
    // competing with the TVs that are actually playing.
    render(<VideoPreview video={video({ filename: "promo.mp4" })} />);

    const player = screen.getByLabelText("Preview of promo.mp4");
    expect(player).toHaveAttribute("preload", "metadata");
    expect(player).not.toHaveAttribute("autoplay");
  });

  it("shows a still as an image rather than an empty video player", () => {
    // A <video> pointed at a PNG renders a permanently blank frame with
    // useless transport controls.
    render(<VideoPreview video={video({ filename: "logo.png", kind: "Image", duration_secs: null })} />);

    const preview = screen.getByAltText("Preview of logo.png");
    expect(preview.tagName).toBe("IMG");
    expect(preview).toHaveAttribute("src", "/videos/logo.png");
  });

  it("swaps element type when moving between an image and a video", () => {
    const { rerender } = render(
      <VideoPreview video={video({ filename: "logo.png", kind: "Image", duration_secs: null })} />,
    );
    expect(screen.getByAltText("Preview of logo.png").tagName).toBe("IMG");

    rerender(<VideoPreview video={video({ filename: "promo.mp4" })} />);
    expect(screen.getByLabelText("Preview of promo.mp4").tagName).toBe("VIDEO");
    expect(screen.queryByAltText("Preview of logo.png")).not.toBeInTheDocument();
  });

  it("rebuilds the player when the selection changes", () => {
    const { rerender } = render(<VideoPreview video={video({ filename: "a.mp4" })} />);
    const first = screen.getByLabelText("Preview of a.mp4");

    rerender(<VideoPreview video={video({ filename: "b.mp4" })} />);
    const second = screen.getByLabelText("Preview of b.mp4");

    // A reused element would keep the previous file's buffer and playhead.
    expect(second).not.toBe(first);
    expect(second).toHaveAttribute("src", "/videos/b.mp4");
  });
});
