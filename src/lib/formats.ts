/**
 * Turns rows into text for the clipboard or a file (CSV, TSV, Markdown, JSON,
 * SQL), and parses spreadsheet text back into rows. SQL quoting mirrors
 * src-tauri/src/dialect.rs so copied statements run on the engine they came from.
 */
import type { Cell, DriverKind } from "./api";

export type CopyFormat = "csv" | "tsv" | "markdown" | "json" | "insert" | "update";

export const COPY_FORMATS: { id: CopyFormat; label: string }[] = [
  { id: "csv", label: "CSV" },
  { id: "tsv", label: "TSV (spreadsheet)" },
  { id: "markdown", label: "Markdown table" },
  { id: "json", label: "JSON array" },
  { id: "insert", label: "SQL INSERT" },
  { id: "update", label: "SQL UPDATE" },
];

/** Rows to format; `undefined` cells (a new row's DEFAULT) are treated as NULL. */
export interface RowData {
  columns: string[];
  rows: (Cell | undefined)[][];
}

/** Where SQL statements point: `schema` may be empty. */
export interface SqlTarget {
  driver: DriverKind;
  schema: string;
  table: string;
  primaryKey?: string[];
  /** An already quoted table reference, used instead of `schema`/`table`. */
  tableRef?: string;
}

// ---- SQL quoting -----------------------------------------------------------

export function ident(driver: DriverKind, name: string): string {
  if (driver === "mysql") return "`" + name.replace(/`/g, "``") + "`";
  if (driver === "bigquery") return "`" + name.replace(/\\/g, "\\\\").replace(/`/g, "\\`") + "`";
  return `"${name.replace(/"/g, '""')}"`;
}

export function tableName(driver: DriverKind, schema: string, table: string): string {
  return schema ? `${ident(driver, schema)}.${ident(driver, table)}` : ident(driver, table);
}

const MYSQL_ESCAPES: Record<string, string> = { "'": "''", "\\": "\\\\", "\0": "\\0", "\n": "\\n", "\r": "\\r", "\x1a": "\\Z" };
const BIGQUERY_ESCAPES: Record<string, string> = { "'": "\\'", "\\": "\\\\", "\n": "\\n", "\r": "\\r" };

export function literal(driver: DriverKind, value: Cell | undefined): string {
  if (value === null || value === undefined) return "NULL";
  if (driver === "mysql") return `'${value.replace(/['\\\0\n\r\x1a]/g, (c) => MYSQL_ESCAPES[c])}'`;
  if (driver === "bigquery") return `'${value.replace(/['\\\n\r]/g, (c) => BIGQUERY_ESCAPES[c])}'`;
  return `'${value.replace(/'/g, "''")}'`;
}

// ---- formats ---------------------------------------------------------------

/** CSV/TSV field: quoted only when it holds the delimiter, a quote or a line break. NULL is empty. */
function field(value: Cell | undefined, delimiter: string): string {
  if (value === null || value === undefined) return "";
  return value.includes(delimiter) || /["\n\r]/.test(value) ? `"${value.replace(/"/g, '""')}"` : value;
}

export function toDelimited(data: RowData, delimiter: "," | "\t"): string {
  const line = (cells: (Cell | undefined)[]) => cells.map((v) => field(v, delimiter)).join(delimiter);
  return [line(data.columns), ...data.rows.map(line)].join("\n");
}

export function toMarkdown(data: RowData): string {
  const cell = (v: Cell | undefined) =>
    v === null || v === undefined ? "NULL" : v.replace(/\\/g, "\\\\").replace(/\|/g, "\\|").replace(/\r?\n/g, "<br>");
  const line = (cells: string[]) => `| ${cells.join(" | ")} |`;
  return [
    line(data.columns.map(cell)),
    line(data.columns.map(() => "---")),
    ...data.rows.map((r) => line(data.columns.map((_, i) => cell(r[i])))),
  ].join("\n");
}

export function toJson(data: RowData): string {
  return JSON.stringify(
    data.rows.map((r) => Object.fromEntries(data.columns.map((c, i) => [c, r[i] ?? null]))),
    null,
    2,
  );
}

export function toInserts(data: RowData, target: SqlTarget): string {
  const t = target.tableRef ?? tableName(target.driver, target.schema, target.table);
  const cols = data.columns.map((c) => ident(target.driver, c)).join(", ");
  return data.rows
    .map((r) => `INSERT INTO ${t} (${cols}) VALUES (${data.columns.map((_, i) => literal(target.driver, r[i])).join(", ")});`)
    .join("\n");
}

/** UPDATE statements that set every other column, matched on the primary key. */
export function toUpdates(data: RowData, target: SqlTarget): string {
  const key = (target.primaryKey ?? []).map((k) => data.columns.indexOf(k)).filter((i) => i >= 0);
  if (!key.length) throw new Error("UPDATE statements need a primary key");
  const rest = data.columns.map((_, i) => i).filter((i) => !key.includes(i));
  const t = target.tableRef ?? tableName(target.driver, target.schema, target.table);
  const q = (i: number) => ident(target.driver, data.columns[i]);
  return data.rows
    .map((r) => {
      const set = (rest.length ? rest : key).map((i) => `${q(i)} = ${literal(target.driver, r[i])}`).join(", ");
      const where = key
        .map((i) => (r[i] == null ? `${q(i)} IS NULL` : `${q(i)} = ${literal(target.driver, r[i])}`))
        .join(" AND ");
      return `UPDATE ${t} SET ${set} WHERE ${where};`;
    })
    .join("\n");
}

export function formatRows(format: CopyFormat, data: RowData, target: SqlTarget): string {
  switch (format) {
    case "csv": return toDelimited(data, ",");
    case "tsv": return toDelimited(data, "\t");
    case "markdown": return toMarkdown(data);
    case "json": return toJson(data);
    case "insert": return toInserts(data, target);
    case "update": return toUpdates(data, target);
  }
}

/**
 * A best guess at the table a query reads from, already in SQL form, for
 * INSERT statements built from its results.
 */
export function guessTable(sql: string): string | undefined {
  return /\bfrom\s+((?:[\w$]+|"[^"]+"|`[^`]+`|\[[^\]]+\])(?:\.(?:[\w$]+|"[^"]+"|`[^`]+`|\[[^\]]+\]))*)/i.exec(sql)?.[1];
}

// ---- pasting ---------------------------------------------------------------

/**
 * Parses spreadsheet or CSV text into rows of fields. Tabs win over commas as
 * the delimiter; quoted fields may hold delimiters, quotes and line breaks.
 */
export function parseDelimited(text: string): string[][] {
  const firstLine = text.split(/\r?\n/, 1)[0] ?? "";
  const delimiter = firstLine.includes("\t") || !firstLine.includes(",") ? "\t" : ",";
  const rows: string[][] = [];
  let row: string[] = [];
  let value = "";
  let quoted = false;
  let i = 0;
  const endField = () => (row.push(value), (value = ""));
  const endRow = () => (endField(), rows.push(row), (row = []));
  while (i < text.length) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') (value += '"'), (i += 2);
      else if (ch === '"') (quoted = false), i++;
      else (value += ch), i++;
    } else if (ch === '"' && value === "") (quoted = true), i++;
    else if (ch === delimiter) endField(), i++;
    else if (ch === "\r" && text[i + 1] === "\n") endRow(), (i += 2);
    else if (ch === "\n" || ch === "\r") endRow(), i++;
    else (value += ch), i++;
  }
  if (value !== "" || row.length) endRow();
  return rows;
}

/**
 * Lines up pasted text with the table's columns: by name when the first line
 * is a header, otherwise by position. Empty fields are left to the column's
 * DEFAULT (`undefined`) and a bare `NULL` becomes NULL.
 */
export function pastedRows(text: string, columns: string[]): { rows: (Cell | undefined)[][]; byName: boolean } {
  const lines = parseDelimited(text).filter((r) => r.some((v) => v.trim() !== ""));
  if (!lines.length) return { rows: [], byName: false };
  const lower = columns.map((c) => c.toLowerCase());
  const header = lines[0].map((h) => lower.indexOf(h.trim().toLowerCase()));
  const byName = header.every((i) => i >= 0) && new Set(header).size === header.length;
  const targets = byName ? header : columns.map((_, i) => i);
  const rows = (byName ? lines.slice(1) : lines).map((line) => {
    const out: (Cell | undefined)[] = new Array(columns.length).fill(undefined);
    line.forEach((v, i) => {
      const col = targets[i];
      if (col === undefined || col >= columns.length || v === "") return;
      out[col] = v === "NULL" ? null : v;
    });
    return out;
  });
  return { rows, byName };
}

/**
 * Lines up a JSON array of objects (as Export writes it) with the table's
 * columns by key, case-insensitively. Missing keys are left to the column's
 * DEFAULT; numbers and booleans become text, nested values JSON. Numbers keep
 * their source text where the engine exposes it, and a number a double can't
 * hold exactly is refused rather than rounded. Returns the keys that matched
 * no column.
 */
export function jsonRows(text: string, columns: string[]): { rows: (Cell | undefined)[][]; unknown: string[] } {
  const json = JSON as { rawJSON?: (text: string) => unknown; isRawJSON?: (v: unknown) => v is { rawJSON: string } };
  const data: unknown = JSON.parse(text, (key, v, context?: { source?: string }) => {
    if (typeof v !== "number") return v;
    if (json.rawJSON && context?.source !== undefined) return json.rawJSON(context.source);
    if (Number.isInteger(v) && !Number.isSafeInteger(v)) {
      throw new Error(`"${key}" holds a number too large to import exactly; quote it as a string.`);
    }
    return v;
  });
  const cell = (v: unknown): Cell =>
    v === null ? null : typeof v !== "object" ? String(v) : json.isRawJSON?.(v) ? v.rawJSON : JSON.stringify(v);
  if (!Array.isArray(data) || data.some((d) => typeof d !== "object" || d === null || Array.isArray(d))) {
    throw new Error("Expected a JSON array of objects, one per row.");
  }
  const lower = columns.map((c) => c.toLowerCase());
  const unknown = new Set<string>();
  const rows = (data as Record<string, unknown>[]).map((obj) => {
    const out: (Cell | undefined)[] = new Array(columns.length).fill(undefined);
    for (const [key, v] of Object.entries(obj)) {
      const col = lower.indexOf(key.toLowerCase());
      if (col < 0) unknown.add(key);
      else out[col] = cell(v);
    }
    return out;
  });
  if (rows.length && rows.every((r) => r.every((v) => v === undefined))) {
    throw new Error(`No key matches a column of this table${unknown.size ? ` (found ${[...unknown].join(", ")})` : ""}.`);
  }
  return { rows, unknown: [...unknown] };
}
