import { useEffect, useState } from "react";
import { thumbnailUrl } from "../api";
import "./Thumbnail.css";

export interface ThumbnailProps {
  /** Video filename, or `null` when nothing is playing. */
  filename: string | null;
  /** Extra class for sizing — the caller decides how big it renders. */
  className?: string;
}

/**
 * A video's generated poster frame.
 *
 * The server 404s when ffmpeg was missing or could not decode the file, which
 * is a supported state rather than an error — so a failure falls back to an
 * empty box of the same size rather than the browser's broken-image icon. The
 * box keeps the layout from reflowing between videos that have a poster and
 * videos that do not.
 *
 * `alt=""` is deliberate: every caller renders the filename right next to it,
 * so announcing the name twice is noise to a screen reader.
 */
export function Thumbnail({ filename, className }: ThumbnailProps) {
  const [failed, setFailed] = useState(false);

  // A tile's filename changes as playback moves between videos. Without this
  // reset, one 404 would leave the placeholder in place for every later video.
  useEffect(() => setFailed(false), [filename]);

  const classes = `thumb${className ? ` ${className}` : ""}`;

  if (filename === null || failed) {
    return <span className={`${classes} thumb--empty`} aria-hidden="true" />;
  }

  return (
    <img
      className={classes}
      src={thumbnailUrl(filename)}
      alt=""
      // A large library would otherwise fetch every poster on first paint.
      loading="lazy"
      decoding="async"
      onError={() => setFailed(true)}
    />
  );
}
