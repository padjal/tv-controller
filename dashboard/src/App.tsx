import { useCallback, useState } from "react";
import { TVGrid } from "./components/TVGrid";
import { VideoLibrary } from "./components/VideoLibrary";

/**
 * Owns the selection that CommandBar acts on. Device and video lists live in
 * the components that render them.
 *
 * CommandBar arrives in task 4.5.
 */
export function App() {
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [selectedVideoId, setSelectedVideoId] = useState<string | null>(null);

  const toggle = useCallback((id: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (!next.delete(id)) {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Stable, so VideoLibrary's "selected file vanished" effect does not re-run
  // on every render.
  const selectVideo = useCallback((videoId: string | null) => setSelectedVideoId(videoId), []);

  return (
    <main className="app">
      <header className="app__header">
        <h1>TV Controller</h1>
        <span className="app__status">
          {selectedIds.size > 0 ? `${selectedIds.size} selected` : "none selected"}
        </span>
      </header>

      <div className="app__layout">
        <TVGrid selectedIds={selectedIds} onToggle={toggle} />
        <VideoLibrary selectedVideoId={selectedVideoId} onSelect={selectVideo} />
      </div>
    </main>
  );
}
