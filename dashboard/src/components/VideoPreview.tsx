import { videoFileUrl } from "../api";
import type { Video } from "../types";
import "./VideoPreview.css";

export interface VideoPreviewProps {
  /** The video to preview, or `null` when nothing is selected. */
  video: Video | null;
}

/**
 * A small player for the selected video, so an operator can see what a file
 * actually contains before putting it on a wall of TVs.
 *
 * The server already serves the file over `/videos` with Range support, so
 * this needs nothing new from the backend — a plain `<video>` element seeks
 * and scrubs against the same bytes the Pis stream.
 *
 * Deliberately *not* autoplaying and `preload="metadata"`: the dashboard may
 * be open on several machines at once, and eagerly buffering every selected
 * file would put a multi-megabit stream on the LAN for a video nobody asked to
 * watch — competing with the TVs that are actually playing. Metadata alone is
 * a few KB and is enough for the browser to show a first frame and a duration.
 */
export function VideoPreview({ video }: VideoPreviewProps) {
  if (video === null) {
    return (
      <p className="preview__note" data-testid="preview-empty">
        Select a video to preview it here.
      </p>
    );
  }

  return (
    <figure className="preview">
      {/* Keyed on the id so switching selection tears the old element down
          rather than mutating src on a player that is mid-download — that
          leaves the previous video's buffer and position in place. */}
      <video
        key={video.id}
        className="preview__player"
        src={videoFileUrl(video.filename)}
        preload="metadata"
        controls
        playsInline
        aria-label={`Preview of ${video.filename}`}
      />
      <figcaption className="preview__caption" title={video.filename}>
        {video.filename}
      </figcaption>
    </figure>
  );
}
