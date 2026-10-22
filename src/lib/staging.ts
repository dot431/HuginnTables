import type { Cell, CellValue, ChangeSet, RowChange } from "./api";

/**
 * Edits made in the grid that haven't been committed yet. Row indexes below
 * `rows.length` refer to the loaded page; higher ones are new rows.
 */
export interface Staging {
  edits: ReadonlyMap<number, ReadonlyMap<number, Cell>>;
  deleted: ReadonlySet<number>;
  /** New rows; `undefined` cells are left out of the INSERT so defaults apply. */
  inserted: readonly (readonly (Cell | undefined)[])[];
}

export type CellState = "edited" | "inserted" | "deleted" | undefined;

export const emptyStaging = (): Staging => ({ edits: new Map(), deleted: new Set(), inserted: [] });

export function pendingCount(s: Staging): number {
  return s.edits.size + s.deleted.size + s.inserted.length;
}

export function cellAt(
  s: Staging,
  rows: readonly Cell[][],
  col: number,
  row: number,
): { value: Cell | undefined; state: CellState } {
  if (row >= rows.length) {
    return { value: s.inserted[row - rows.length]?.[col], state: "inserted" };
  }
  if (s.deleted.has(row)) return { value: rows[row][col], state: "deleted" };
  const rowEdits = s.edits.get(row);
  if (rowEdits?.has(col)) return { value: rowEdits.get(col)!, state: "edited" };
  return { value: rows[row][col], state: undefined };
}

export function applyEdit(s: Staging, rows: readonly Cell[][], col: number, row: number, value: Cell): Staging {
  if (row >= rows.length) {
    const idx = row - rows.length;
    const inserted = s.inserted.map((r, i) => (i === idx ? r.map((v, c) => (c === col ? value : v)) : r));
    return { ...s, inserted };
  }
  if (s.deleted.has(row)) return s;
  const original = rows[row][col];
  const edits = new Map(s.edits);
  const rowEdits = new Map(edits.get(row) ?? []);
  if (value === original) rowEdits.delete(col);
  else rowEdits.set(col, value);
  if (rowEdits.size) edits.set(row, rowEdits);
  else edits.delete(row);
  return { ...s, edits };
}

/** Marks page rows for deletion (or un-marks them) and drops new rows. */
export function toggleDelete(s: Staging, rowsLength: number, targets: number[]): Staging {
  const pageRows = targets.filter((r) => r < rowsLength);
  const newRows = new Set(targets.filter((r) => r >= rowsLength).map((r) => r - rowsLength));
  const deleted = new Set(s.deleted);
  const allDeleted = pageRows.length > 0 && pageRows.every((r) => deleted.has(r));
  const edits = new Map(s.edits);
  for (const r of pageRows) {
    if (allDeleted) deleted.delete(r);
    else {
      deleted.add(r);
      edits.delete(r);
    }
  }
  return { edits, deleted, inserted: s.inserted.filter((_, i) => !newRows.has(i)) };
}

export function addRow(s: Staging, width: number): Staging {
  return { ...s, inserted: [...s.inserted, new Array(width).fill(undefined)] };
}

export function revertRows(s: Staging, rowsLength: number, targets: number[]): Staging {
  const edits = new Map(s.edits);
  const deleted = new Set(s.deleted);
  for (const r of targets) {
    edits.delete(r);
    deleted.delete(r);
  }
  const newRows = new Set(targets.filter((r) => r >= rowsLength).map((r) => r - rowsLength));
  return { edits, deleted, inserted: s.inserted.filter((_, i) => !newRows.has(i)) };
}

/**
 * Turns staged edits into a change set. Rows are identified by the primary
 * key when there is one, otherwise by every original value.
 */
export function buildChangeSet(
  s: Staging,
  rows: readonly Cell[][],
  columns: readonly string[],
  primaryKey: readonly string[],
  schema: string,
  table: string,
): ChangeSet {
  const keyIdx = (primaryKey.length ? primaryKey : columns).map((k) => columns.indexOf(k)).filter((i) => i >= 0);
  const keyOf = (row: number): CellValue[] => keyIdx.map((i) => ({ column: columns[i], value: rows[row][i] }));
  const changes: RowChange[] = [];

  for (const [row, rowEdits] of [...s.edits].sort((a, b) => a[0] - b[0])) {
    if (s.deleted.has(row)) continue;
    changes.push({
      kind: "update",
      key: keyOf(row),
      values: [...rowEdits].sort((a, b) => a[0] - b[0]).map(([c, value]) => ({ column: columns[c], value })),
    });
  }
  for (const row of [...s.deleted].sort((a, b) => a - b)) {
    changes.push({ kind: "delete", key: keyOf(row) });
  }
  for (const values of s.inserted) {
    changes.push({
      kind: "insert",
      values: values.flatMap((value, c) => (value === undefined ? [] : [{ column: columns[c], value }])),
    });
  }
  return { schema, table, keyIsPrimary: primaryKey.length > 0, changes };
}
