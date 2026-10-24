import { useCallback, useEffect, useState } from "react";

/** How a table's columns are laid out in the grid, remembered per table. */
export interface ColumnLayout {
  /** Widths the user dragged, by column name. */
  widths: Record<string, number>;
  /** Names of hidden columns. */
  hidden: string[];
  /** How many of the visible columns stay pinned on the left. */
  frozen: number;
}

const PREFIX = "huginn.columns.";

const emptyLayout = (): ColumnLayout => ({ widths: {}, hidden: [], frozen: 0 });

function isEmpty(l: ColumnLayout): boolean {
  return !Object.keys(l.widths).length && !l.hidden.length && !l.frozen;
}

function load(key: string): ColumnLayout {
  try {
    const saved = JSON.parse(localStorage.getItem(PREFIX + key) ?? "null");
    if (!saved || typeof saved !== "object") return emptyLayout();
    return {
      widths: saved.widths && typeof saved.widths === "object" ? saved.widths : {},
      hidden: Array.isArray(saved.hidden) ? saved.hidden.filter((h: unknown) => typeof h === "string") : [],
      frozen: Number.isInteger(saved.frozen) && saved.frozen > 0 ? saved.frozen : 0,
    };
  } catch {
    return emptyLayout();
  }
}

function save(key: string, layout: ColumnLayout) {
  try {
    if (isEmpty(layout)) localStorage.removeItem(PREFIX + key);
    else localStorage.setItem(PREFIX + key, JSON.stringify(layout));
  } catch {
    /* storage unavailable */
  }
}

/** The remembered column layout for `key`, and a way to change it. */
export function useColumnLayout(key: string) {
  const [layout, setLayout] = useState<ColumnLayout>(() => load(key));

  useEffect(() => setLayout(load(key)), [key]);

  const update = useCallback(
    (change: (l: ColumnLayout) => ColumnLayout) =>
      setLayout((l) => {
        const next = change(l);
        save(key, next);
        return next;
      }),
    [key],
  );

  return [layout, update] as const;
}
