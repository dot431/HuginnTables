import { useEffect, useMemo, useState } from "react";
import {
  clearHistory,
  defaultName,
  deleteSaved,
  formatDuration,
  HistoryEntry,
  relativeTime,
  removeHistoryEntry,
  SavedQuery,
  saveQuery,
  updateSaved,
  useQueryLibrary,
} from "../lib/history";
import { SqlEditorHandle } from "./SqlEditor";
import { ContextMenu, MenuItem, MenuState, toast, useDragSize } from "./ui";

type Section = "history" | "saved";

const PREFS_KEY = "huginn.historyPanel";

function readPrefs(): { open: boolean; section: Section } {
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}");
    return { open: p.open === true, section: p.section === "saved" ? "saved" : "history" };
  } catch {
    return { open: false, section: "history" };
  }
}

function writePrefs(change: Partial<{ open: boolean; section: Section }>) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ ...readPrefs(), ...change }));
  } catch {
    /* storage unavailable */
  }
}

/** Open state and width of a query tab's history panel; ⌘Y toggles it while the tab is active. */
export function useHistoryPanel(active: boolean) {
  const [open, setOpen] = useState(() => readPrefs().open);
  const drag = useDragSize(300, 220, 640, "x", true);

  useEffect(() => writePrefs({ open }), [open]);

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      // Ctrl+Y is redo in the editor on Windows and Linux; the editor claims it first.
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "y" && !e.defaultPrevented) {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active]);

  return { open, toggle: () => setOpen((v) => !v), close: () => setOpen(false), ...drag };
}

export function HistoryPanel(props: {
  connectionId: string;
  editor: React.MutableRefObject<SqlEditorHandle>;
  width: number;
  dragging: boolean;
  onResize: (e: React.MouseEvent) => void;
  /** Runs everything in the editor, as ⇧⌘↵ does. */
  onRunAll: () => void;
  onClose: () => void;
}) {
  const { history, saved } = useQueryLibrary(props.connectionId);
  const [section, setSection] = useState<Section>(() => readPrefs().section);
  const [search, setSearch] = useState("");
  const [editing, setEditing] = useState<{ id: string; name: string } | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [now, setNow] = useState(Date.now());

  // Keep relative times fresh.
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(t);
  }, []);
  useEffect(() => setNow(Date.now()), [history]);

  const switchSection = (s: Section) => {
    setSection(s);
    writePrefs({ section: s });
  };

  const q = search.trim().toLowerCase();
  const visibleHistory = useMemo(
    () => (q ? history.filter((e) => e.sql.toLowerCase().includes(q) || e.error?.toLowerCase().includes(q)) : history),
    [history, q],
  );
  const visibleSaved = useMemo(
    () =>
      saved
        .filter((s) => !q || s.name.toLowerCase().includes(q) || s.sql.toLowerCase().includes(q))
        .sort((a, b) => a.name.localeCompare(b.name)),
    [saved, q],
  );
  const savedBySql = useMemo(() => new Map(saved.map((s) => [s.sql, s])), [saved]);

  // Editor changes go through a transaction, so ⌘Z brings back what was there.
  const load = (sql: string) => {
    const view = props.editor.current.view;
    if (!view) return;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: sql }, selection: { anchor: sql.length }, scrollIntoView: true });
    view.focus();
  };

  const insert = (sql: string) => {
    const view = props.editor.current.view;
    if (!view) return;
    view.dispatch({ ...view.state.replaceSelection(sql), scrollIntoView: true });
    view.focus();
  };

  const loadAndRun = (sql: string) => {
    load(sql);
    props.onRunAll();
  };

  const copy = (sql: string) => navigator.clipboard.writeText(sql);

  const save = (sql: string) => {
    const { query, persisted } = saveQuery(props.connectionId, defaultName(sql), sql);
    if (!persisted) toast("Saved for this session only: local storage is unavailable or full.", "error");
    switchSection("saved");
    setSearch("");
    setEditing({ id: query.id, name: query.name });
  };

  const show = (s: SavedQuery) => {
    switchSection("saved");
    setSearch(s.name);
  };

  /** Saves the selection, or the whole editor when nothing is selected. */
  const saveEditor = () => {
    const view = props.editor.current.view;
    if (!view) return;
    const sel = view.state.selection.main;
    const text = (sel.empty ? view.state.doc.toString() : view.state.sliceDoc(sel.from, sel.to)).trim();
    if (!text) toast("The editor is empty.");
    else save(text);
  };

  const commitRename = () => {
    if (!editing) return;
    const name = editing.name.trim();
    if (name) updateSaved(props.connectionId, editing.id, { name });
    setEditing(null);
  };

  const remove = (s: SavedQuery) => {
    if (window.confirm(`Delete the saved query “${s.name}”?`)) deleteSaved(props.connectionId, s.id);
  };

  const replaceWithEditor = (s: SavedQuery) => {
    const text = props.editor.current.view?.state.doc.toString().trim();
    if (!text) toast("The editor is empty.");
    else if (window.confirm(`Replace the SQL of “${s.name}” with the editor's content?`)) updateSaved(props.connectionId, s.id, { sql: text });
  };

  const openMenu = (e: React.MouseEvent, sql: string, extra: MenuItem[]) => {
    e.preventDefault();
    setMenu({
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: "Load into editor", action: () => load(sql) },
        { label: "Insert at cursor", action: () => insert(sql) },
        { label: "Load and run", action: () => loadAndRun(sql) },
        { separator: true, label: "" },
        { label: "Copy SQL", action: () => copy(sql) },
        ...extra,
      ],
    });
  };

  const historyItem = (e: HistoryEntry) => {
    const savedAs = savedBySql.get(e.sql);
    return (
      <div
        key={e.id}
        className={`hp-item ${e.error ? "err" : ""}`}
        onClick={() => load(e.sql)}
        onContextMenu={(ev) =>
          openMenu(ev, e.sql, [
            savedAs ? { label: "Show saved query", action: () => show(savedAs) } : { label: "Save query", action: () => save(e.sql) },
            { separator: true, label: "" },
            { label: "Remove from history", action: () => removeHistoryEntry(props.connectionId, e.id) },
          ])
        }
        title={e.error}
      >
        <div className="hp-sql">{e.sql.slice(0, 600)}</div>
        <div className="hp-meta">
          {e.error ? (
            <span className="hp-err">Error</span>
          ) : (
            <span>
              {e.hasRows ? `${e.rows.toLocaleString()} row${e.rows === 1 ? "" : "s"}` : `${e.affected.toLocaleString()} affected`}
            </span>
          )}
          <span>{formatDuration(e.durationMs)}</span>
          {e.statements > 1 && <span>{e.statements} statements</span>}
          {e.runs > 1 && <span title="Consecutive identical runs">×{e.runs}</span>}
          <div className="spacer" />
          <span title={new Date(e.at).toLocaleString()}>{relativeTime(e.at, now)}</span>
        </div>
        <div className="hp-actions" onClick={(ev) => ev.stopPropagation()}>
          <button className="ghost icon" title="Insert at cursor" onClick={() => insert(e.sql)}>↧</button>
          <button className="ghost icon" title="Copy SQL" onClick={() => copy(e.sql)}>⧉</button>
          <button
            className={`ghost icon ${savedAs ? "starred" : ""}`}
            title={savedAs ? `Saved as “${savedAs.name}”` : "Save query"}
            onClick={() => (savedAs ? show(savedAs) : save(e.sql))}
          >
            {savedAs ? "★" : "☆"}
          </button>
        </div>
      </div>
    );
  };

  const savedItem = (s: SavedQuery) => (
    <div
      key={s.id}
      className="hp-item"
      onClick={() => editing?.id !== s.id && load(s.sql)}
      onContextMenu={(ev) =>
        openMenu(ev, s.sql, [
          { label: "Rename", action: () => setEditing({ id: s.id, name: s.name }) },
          { label: "Replace with editor content", action: () => replaceWithEditor(s) },
          { separator: true, label: "" },
          { label: "Delete", action: () => remove(s) },
        ])
      }
    >
      {editing?.id === s.id ? (
        <input
          className="hp-rename"
          autoFocus
          value={editing.name}
          onFocus={(e) => e.currentTarget.select()}
          onClick={(e) => e.stopPropagation()}
          onChange={(e) => setEditing({ id: s.id, name: e.target.value })}
          onBlur={commitRename}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitRename();
            if (e.key === "Escape") setEditing(null);
          }}
        />
      ) : (
        <div className="hp-name">{s.name}</div>
      )}
      <div className="hp-sql">{s.sql.slice(0, 600)}</div>
      <div className="hp-meta">
        <div className="spacer" />
        <span title={`Saved ${new Date(s.createdAt).toLocaleString()}`}>
          {s.updatedAt > s.createdAt ? "edited" : "saved"} {relativeTime(s.updatedAt, now)}
        </span>
      </div>
      <div className="hp-actions" onClick={(ev) => ev.stopPropagation()}>
        <button className="ghost icon" title="Insert at cursor" onClick={() => insert(s.sql)}>↧</button>
        <button className="ghost icon" title="Copy SQL" onClick={() => copy(s.sql)}>⧉</button>
        <button className="ghost icon" title="Rename" onClick={() => setEditing({ id: s.id, name: s.name })}>✎</button>
        <button className="ghost icon" title="Delete" onClick={() => remove(s)}>×</button>
      </div>
    </div>
  );

  const list = section === "history" ? visibleHistory : visibleSaved;
  const total = section === "history" ? history.length : saved.length;

  return (
    <div className="history-panel" style={{ width: props.width }}>
      <div className={`hp-resizer ${props.dragging ? "dragging" : ""}`} onMouseDown={props.onResize} />
      <div className="hp-head">
        <div className="segmented">
          <button className={section === "history" ? "on" : ""} onClick={() => switchSection("history")}>History</button>
          <button className={section === "saved" ? "on" : ""} onClick={() => switchSection("saved")}>
            Saved{saved.length ? ` (${saved.length})` : ""}
          </button>
        </div>
        <div className="spacer" />
        {section === "saved" ? (
          <button className="ghost" onClick={saveEditor} title="Save the selection, or the whole editor">☆ Save editor</button>
        ) : (
          <button
            className="ghost"
            disabled={!history.length}
            onClick={() => window.confirm("Clear the query history of this connection?") && clearHistory(props.connectionId)}
          >
            Clear
          </button>
        )}
        <button className="ghost icon" onClick={props.onClose} title="Close (⌘Y)">×</button>
      </div>
      <input
        className="hp-search"
        autoFocus
        placeholder={section === "history" ? "Search history" : "Search saved queries"}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            if (search) setSearch("");
            else props.editor.current.view?.focus();
          }
        }}
      />
      <div className="hp-list">
        {section === "history" ? visibleHistory.map(historyItem) : visibleSaved.map(savedItem)}
        {list.length === 0 && (
          <div className="faint hp-empty">
            {total > 0
              ? "No matches"
              : section === "history"
                ? "Queries you run on this connection show up here."
                : "Star a history entry, or save what's in the editor, to keep it here."}
          </div>
        )}
      </div>
      <div className="hp-foot faint">Click loads into the editor (⌘Z undoes) · right-click for more</div>
      <ContextMenu menu={menu} onClose={() => setMenu(null)} />
    </div>
  );
}
