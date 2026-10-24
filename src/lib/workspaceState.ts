import type { FetchRequest } from "./api";
import type { FilterState } from "../components/FilterBar";

/** How a table tab was being viewed: what Workspace needs to reopen it the same way. */
export interface TableView {
  filter: FilterState;
  sort: FetchRequest["sort"];
  limit: number;
}

export type SavedTab =
  | { kind: "table"; schema: string; table: string; filter?: FilterState; sort?: FetchRequest["sort"]; limit?: number }
  | { kind: "query"; title: string; slot: number; sql?: string };

/** What was open for one saved connection. No row data or staged edits. */
export interface SavedWorkspace {
  database: string;
  schema: string;
  tabs: SavedTab[];
  /** Index into `tabs`, or -1 for none. */
  active: number;
}

const KEY_PREFIX = "huginn.workspace.";
const REOPEN_KEY = "huginn.reopenTabs";

export function reopenTabsEnabled(): boolean {
  try {
    return localStorage.getItem(REOPEN_KEY) !== "0";
  } catch {
    return true;
  }
}

export function setReopenTabs(on: boolean) {
  try {
    localStorage.setItem(REOPEN_KEY, on ? "1" : "0");
  } catch {
    // Storage unavailable: the default (on) applies next time.
  }
}

const isFilter = (f: unknown): f is FilterState =>
  !!f && typeof f === "object" && Array.isArray((f as FilterState).filters) && typeof (f as FilterState).rawWhere === "string";

const isSort = (s: unknown): s is FetchRequest["sort"] =>
  s === null || (!!s && typeof s === "object" && typeof (s as { column: unknown }).column === "string");

/** Reads what was open last time, dropping anything malformed. */
export function loadWorkspace(connectionId: string): SavedWorkspace | null {
  try {
    const raw = localStorage.getItem(KEY_PREFIX + connectionId);
    if (!raw) return null;
    const v = JSON.parse(raw);
    if (!v || !Array.isArray(v.tabs)) return null;
    const tabs: SavedTab[] = [];
    let active = -1;
    v.tabs.forEach((t: Record<string, unknown>, i: number) => {
      if (t?.kind === "table" && typeof t.schema === "string" && typeof t.table === "string") {
        tabs.push({
          kind: "table",
          schema: t.schema,
          table: t.table,
          filter: isFilter(t.filter) ? t.filter : undefined,
          sort: isSort(t.sort) ? t.sort : undefined,
          limit: typeof t.limit === "number" ? t.limit : undefined,
        });
      } else if (t?.kind === "query" && typeof t.slot === "number") {
        tabs.push({
          kind: "query",
          title: typeof t.title === "string" ? t.title : `Query ${t.slot}`,
          slot: t.slot,
          sql: typeof t.sql === "string" ? t.sql : undefined,
        });
      } else return;
      if (i === v.active) active = tabs.length - 1;
    });
    return {
      database: typeof v.database === "string" ? v.database : "",
      schema: typeof v.schema === "string" ? v.schema : "",
      tabs,
      active,
    };
  } catch {
    return null;
  }
}

export function saveWorkspace(connectionId: string, state: SavedWorkspace) {
  try {
    localStorage.setItem(KEY_PREFIX + connectionId, JSON.stringify(state));
  } catch {
    // Storage unavailable or full: the session just won't be restored.
  }
}
