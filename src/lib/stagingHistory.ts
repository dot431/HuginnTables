import { useCallback, useState } from "react";
import { emptyStaging, Staging } from "./staging";

/** Steps kept for undo; older ones fall off. */
const LIMIT = 200;

interface History {
  past: Staging[];
  present: Staging;
  future: Staging[];
}

/**
 * Staged edits with undo and redo. Passing an updater records an undo step;
 * passing a staging outright replaces it and clears the history, which is
 * what reload, commit and discard do.
 */
export function useStagingHistory() {
  const [history, setHistory] = useState<History>(() => ({ past: [], present: emptyStaging(), future: [] }));

  const setStaging = useCallback((next: Staging | ((s: Staging) => Staging)) => {
    setHistory((h) => {
      if (typeof next !== "function") return { past: [], present: next, future: [] };
      const present = next(h.present);
      if (present === h.present) return h;
      return { past: [...h.past.slice(-(LIMIT - 1)), h.present], present, future: [] };
    });
  }, []);

  const undo = useCallback(() => {
    setHistory((h) =>
      h.past.length
        ? { past: h.past.slice(0, -1), present: h.past[h.past.length - 1], future: [h.present, ...h.future] }
        : h,
    );
  }, []);

  const redo = useCallback(() => {
    setHistory((h) =>
      h.future.length ? { past: [...h.past, h.present], present: h.future[0], future: h.future.slice(1) } : h,
    );
  }, []);

  return {
    staging: history.present,
    setStaging,
    undo,
    redo,
    canUndo: history.past.length > 0,
    canRedo: history.future.length > 0,
  };
}
