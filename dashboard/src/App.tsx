import { useCallback, useState } from "react";
import { TVGrid } from "./components/TVGrid";

/**
 * Owns the selection that CommandBar acts on. Device and video lists live in
 * the components that render them.
 *
 * VideoLibrary and CommandBar arrive in tasks 4.4 and 4.5.
 */
export function App() {
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  const toggle = useCallback((id: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (!next.delete(id)) {
        next.add(id);
      }
      return next;
    });
  }, []);

  return (
    <main className="app">
      <header className="app__header">
        <h1>TV Controller</h1>
        <span className="app__status">
          {selectedIds.size > 0 ? `${selectedIds.size} selected` : "none selected"}
        </span>
      </header>

      <TVGrid selectedIds={selectedIds} onToggle={toggle} />
    </main>
  );
}
