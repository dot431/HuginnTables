import { useEffect, useState } from "react";
import type { DriverKind, QueryResult } from "./api";

/** One run of the query editor: the batch of statements sent together. Results aren't kept. */
export interface HistoryEntry {
  id: string;
  /** The batch as a script that splits back into the same statements. */
  sql: string;
  /** Epoch ms of the latest run. */
  at: number;
  durationMs: number;
  statements: number;
  /** Whether any statement returned rows; then `rows` counts them, otherwise `affected` applies. */
  hasRows: boolean;
  rows: number;
  affected: number;
  error?: string;
  /** Consecutive identical runs folded into this entry. */
  runs: number;
}

export interface SavedQuery {
  id: string;
  name: string;
  sql: string;
  createdAt: number;
  updatedAt: number;
}

const MAX_HISTORY = 500;
const MAX_ERROR = 2000;
const historyKey = (connectionId: string) => `huginn.history.${connectionId}`;
const savedKey = (connectionId: string) => `huginn.saved.${connectionId}`;

// Lists live in memory too, so history keeps working for the session when storage is unavailable.
const cache = new Map<string, unknown[]>();
const listeners = new Map<string, Set<() => void>>();

function read<T>(key: string): T[] {
  let list = cache.get(key);
  if (!list) {
    try {
      const parsed = JSON.parse(localStorage.getItem(key) ?? "[]");
      list = Array.isArray(parsed) ? parsed : [];
    } catch {
      list = [];
    }
    cache.set(key, list);
  }
  return list as T[];
}

/** Stores a list; with `trim`, drops the oldest half until it fits the quota. Returns whether it was persisted. */
function write<T>(connectionId: string, key: string, list: T[], trim = false): boolean {
  cache.set(key, list);
  listeners.get(connectionId)?.forEach((l) => l());
  for (let items = list; ; items = items.slice(0, Math.floor(items.length / 2))) {
    try {
      localStorage.setItem(key, JSON.stringify(items));
      return true;
    } catch {
      if (!trim || items.length <= 1) return false;
    }
  }
}

const newId = () => `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;

export const loadHistory = (connectionId: string) => read<HistoryEntry>(historyKey(connectionId));
export const loadSaved = (connectionId: string) => read<SavedQuery>(savedKey(connectionId));

/** Joins statements back into a script the editor splits the same way. */
export function batchText(statements: string[], driver: DriverKind): string {
  if (driver === "redis" || driver === "mongodb") return statements.join("\n");
  // A line comment at the end would swallow the semicolon.
  return statements.map((s) => (/(--|#)[^\n]*$/.test(s) ? `${s}\n;` : `${s};`)).join("\n");
}

/** Records an executed batch, newest first; a repeat of the latest entry updates it instead. */
export function recordRun(
  connectionId: string,
  driver: DriverKind,
  statements: string[],
  results: QueryResult[],
  durationMs: number,
  error?: string,
) {
  const sql = batchText(statements, driver);
  const failed = error ?? results.find((r) => r.error)?.error;
  const entry: HistoryEntry = {
    id: newId(),
    sql,
    at: Date.now(),
    durationMs: Math.round(durationMs),
    statements: statements.length,
    hasRows: results.some((r) => r.hasRows),
    rows: results.reduce((n, r) => n + (r.hasRows ? r.rows.length : 0), 0),
    affected: results.reduce((n, r) => n + (r.hasRows ? 0 : r.rowsAffected), 0),
    ...(failed ? { error: failed.slice(0, MAX_ERROR) } : {}),
    runs: 1,
  };
  const list = loadHistory(connectionId);
  const top = list[0];
  const next =
    top && top.sql === sql
      ? [{ ...entry, id: top.id, runs: (top.runs ?? 1) + 1 }, ...list.slice(1)]
      : [entry, ...list].slice(0, MAX_HISTORY);
  write(connectionId, historyKey(connectionId), next, true);
}

export function removeHistoryEntry(connectionId: string, id: string) {
  write(connectionId, historyKey(connectionId), loadHistory(connectionId).filter((e) => e.id !== id), true);
}

export function clearHistory(connectionId: string) {
  write(connectionId, historyKey(connectionId), []);
}

/** Saves a query. When storage refuses it, `persisted` is false and it only lives for this session. */
export function saveQuery(connectionId: string, name: string, sql: string): { query: SavedQuery; persisted: boolean } {
  const now = Date.now();
  const query: SavedQuery = { id: newId(), name, sql, createdAt: now, updatedAt: now };
  const persisted = write(connectionId, savedKey(connectionId), [query, ...loadSaved(connectionId)]);
  return { query, persisted };
}

export function updateSaved(connectionId: string, id: string, change: Partial<Pick<SavedQuery, "name" | "sql">>): boolean {
  const list = loadSaved(connectionId).map((q) => (q.id === id ? { ...q, ...change, updatedAt: Date.now() } : q));
  return write(connectionId, savedKey(connectionId), list);
}

export function deleteSaved(connectionId: string, id: string) {
  write(connectionId, savedKey(connectionId), loadSaved(connectionId).filter((q) => q.id !== id));
}

/** History and saved queries of a connection, re-rendering when any tab changes them. */
export function useQueryLibrary(connectionId: string): { history: HistoryEntry[]; saved: SavedQuery[] } {
  const [, bump] = useState(0);
  useEffect(() => {
    const set = listeners.get(connectionId) ?? new Set();
    listeners.set(connectionId, set);
    const l = () => bump((n) => n + 1);
    set.add(l);
    return () => void set.delete(l);
  }, [connectionId]);
  return { history: loadHistory(connectionId), saved: loadSaved(connectionId) };
}

/** A default name for a saved query: its first line of code. */
export function defaultName(sql: string): string {
  const line = sql.split("\n").map((l) => l.trim()).find((l) => l && !l.startsWith("--")) ?? "Query";
  return line.length > 48 ? `${line.slice(0, 47)}…` : line;
}

/** "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago", then the date. */
export function relativeTime(at: number, now = Date.now()): string {
  const s = Math.max(0, (now - at) / 1000);
  if (s < 45) return "just now";
  const m = Math.max(1, Math.floor(s / 60));
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.floor(h / 24);
  if (d === 1) return "yesterday";
  if (d < 7) return `${d} days ago`;
  return new Date(at).toLocaleDateString();
}

export const formatDuration = (ms: number) => (ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`);
