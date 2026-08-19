import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Thumbnail } from "./Thumbnail";

/** The <img>, or null once it has fallen back to the placeholder. */
function image(): HTMLImageElement | null {
  return document.querySelector("img");
}

describe("Thumbnail", () => {
  it("points at the poster the server generated", () => {
    render(<Thumbnail filename="promo.mp4" />);
    expect(image()).toHaveAttribute("src", "/thumbnails/promo.mp4.jpg");
  });

  it("encodes names with spaces and hashes", () => {
    render(<Thumbnail filename="golden times #2.mp4" />);
    expect(image()).toHaveAttribute("src", "/thumbnails/golden%20times%20%232.mp4.jpg");
  });

  it("renders a placeholder when nothing is playing", () => {
    const { container } = render(<Thumbnail filename={null} />);
    expect(image()).toBeNull();
    expect(container.querySelector(".thumb--empty")).toBeInTheDocument();
  });

  it("falls back to a placeholder when the poster 404s", () => {
    // ffmpeg may have been missing, or unable to decode the file. That is a
    // supported state, so it must not show a broken-image icon.
    const { container } = render(<Thumbnail filename="promo.mp4" />);
    fireEvent.error(image()!);

    expect(image()).toBeNull();
    expect(container.querySelector(".thumb--empty")).toBeInTheDocument();
  });

  it("retries for a new video after a failure", () => {
    // A tile's filename changes as playback moves on. One 404 must not leave
    // the placeholder stuck there for every later video.
    const { rerender } = render(<Thumbnail filename="broken.mp4" />);
    fireEvent.error(image()!);
    expect(image()).toBeNull();

    rerender(<Thumbnail filename="fine.mp4" />);
    expect(image()).toHaveAttribute("src", "/thumbnails/fine.mp4.jpg");
  });

  it("is decorative — the filename is always rendered next to it", () => {
    render(<Thumbnail filename="promo.mp4" />);
    expect(image()).toHaveAttribute("alt", "");
    expect(image()).toHaveAttribute("loading", "lazy");
  });
});
