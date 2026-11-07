import { useCallback, useEffect, useRef, useState } from "react";
import { EditorView } from "@codemirror/view";
import { api, DriverKind, errorMessage, isSql, QueryResult } from "../lib/api";
import { isWriteStatement, lacksWhere } from "../lib/sqlSafety";
import { guessTable, ident, RowData, SqlTarget, toDelimited, toInserts, toJson } from "../lib/formats";
import { SplitMode, splitStatements, statementAt } from "../lib/sqlSplit";
import { recordRun } from "../lib/history";
import { copyMenuItems, ExportDialog } from "./CopyExport";
import { DataGrid } from "./DataGrid";
import { HistoryPanel, useHistoryPanel } from "./HistoryPanel";
import { ConfirmStatements } from "./Safety";
import { SqlEditor, SqlEditorHandle } from "./SqlEditor";
import { ContextMenu, MenuState, useDragSize } from "./ui";

export function QueryTab(props: {
  sessionId: string;
  /** Saved connection id; history and saved queries are kept per connection. */
  connectionId: string;
  driver: DriverKind;
  storageKey: string;
  initialSql?: string;
  schema: Record<string, string[]>;
  defaultSchema: string;
  /** Writes ask for confirmation before they run. */
  production?: boolean;
  active: boolean;
  onSchemaChanged: () => void;
  /** The editor text, debounced like the local save. */
  onSqlChange?: (sql: string) => void;
}) {
  const [results, setResults] = useState<QueryResult[]>([]);
  const [current, setCurrent] = useState(0);
  const [running, setRunning] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [confirm, setConfirm] = useState<{ statements: string[]; unbounded: string[]; writes: string[] } | null>(null);
  const [exporting, setExporting] = useState(false);
  const editor = useRef<SqlEditorHandle>({ view: null });
  const split = useDragSize(260, 80, 2000, "y");
  const saveTimer = useRef<number>();
  const side = useHistoryPanel(props.active);

  const initial = useRef(
    props.initialSql ?? (() => {
      try {
        return localStorage.getItem(props.storageKey) ?? "";
      } catch {
        return "";
      }
    })(),
  );

  const persist = (text: string) => {
    window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      try {
        localStorage.setItem(props.storageKey, text);
      } catch {
        /* storage unavailable */
      }
      props.onSqlChange?.(text);
    }, 400);
  };

  const execute = useCallback(
    async (statements: string[]) => {
      if (!statements.length || running) return;
      setRunning(true);
      const started = performance.now();
      const tick = window.setInterval(() => setElapsed(performance.now() - started), 100);
      try {
        const out = await api.runQuery(props.sessionId, statements);
        setResults(out);
        recordRun(props.connectionId, props.driver, statements, out, performance.now() - started);
        // Show the first failure, otherwise the last result that has rows.
        const failed = out.findIndex((r) => r.error);
        const lastRows = out.map((r) => r.hasRows).lastIndexOf(true);
        setCurrent(failed >= 0 ? failed : lastRows >= 0 ? lastRows : out.length - 1);
        const changesCatalog =
          props.driver === "redis" || props.driver === "mongodb" ||
          statements.some((s) => /^\s*(create|alter|drop|rename|truncate)\b/i.test(s));
        if (changesCatalog) props.onSchemaChanged();
      } catch (e) {
        setResults([{ sql: statements.join(";\n"), columns: [], rows: [], hasRows: false, rowsAffected: 0, durationMs: 0, truncated: false, error: errorMessage(e) }]);
        setCurrent(0);
        recordRun(props.connectionId, props.driver, statements, [], performance.now() - started, errorMessage(e));
      } finally {
        window.clearInterval(tick);
        setRunning(false);
      }
    },
    [props.sessionId, running, props.onSchemaChanged],
  );

  /** Runs `statements`, asking first about writes on production and UPDATE/DELETE without WHERE. */
  const runChecked = useCallback(
    (statements: string[]) => {
      if (!statements.length || running) return;
      const unbounded = statements.filter((s) => lacksWhere(s, props.driver));
      const writes = props.production ? statements.filter((s) => isWriteStatement(s, props.driver)) : [];
      if (unbounded.length || writes.length) setConfirm({ statements, unbounded, writes });
      else execute(statements);
    },
    [execute, running, props.driver, props.production],
  );

  const run = useCallback(
    (view: EditorView, all: boolean) => {
      const doc = view.state.doc.toString();
      const sel = view.state.selection.main;
      const mode: SplitMode =
        props.driver === "mysql" ? "mysql" : props.driver === "mongodb" ? "mongo" : props.driver === "redis" ? "redis" : "sql";
      let statements: string[];
      if (all) statements = splitStatements(doc, mode).map((s) => s.text);
      else if (!sel.empty) statements = splitStatements(doc.slice(sel.from, sel.to), mode).map((s) => s.text);
      else {
        const stmt = statementAt(doc, sel.head, mode);
        if (stmt) view.dispatch({ selection: { anchor: stmt.from, head: stmt.to } });
        statements = stmt ? [stmt.text] : [];
      }
      runChecked(statements);
    },
    [runChecked, props.driver],
  );

  useEffect(() => {
    if (!props.active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "." && e.metaKey && running) {
        e.preventDefault();
        api.cancelQuery(props.sessionId);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props.active, running, props.sessionId]);

  useEffect(() => {
    if (props.active) editor.current.view?.focus();
  }, [props.active]);

  const result = results[current];

  /** Where INSERT statements built from the current result point. */
  const sqlTarget = (r: QueryResult): SqlTarget => ({
    driver: props.driver,
    schema: "",
    table: "",
    tableRef: guessTable(r.sql) ?? ident(props.driver, "table_name"),
  });

  const resultData = (r: QueryResult, rows?: number[]): RowData => ({
    columns: r.columns.map((c) => c.name),
    rows: rows ? rows.map((i) => r.rows[i]) : r.rows,
  });

  return (
    <div className={`tab-content ${side.open ? "with-side" : ""}`} style={side.open ? { paddingRight: side.size } : undefined}>
      <div className="toolbar">
        <button
          className="primary"
          disabled={running}
          onClick={() => editor.current.view && run(editor.current.view, false)}
          title="Run statement under cursor or selection (⌘↵)"
        >
          ▶ Run
        </button>
        <button disabled={running} onClick={() => editor.current.view && run(editor.current.view, true)} title="Run all (⇧⌘↵)">
          Run all
        </button>
        {running && (
          <>
            <button className="danger" onClick={() => api.cancelQuery(props.sessionId)} title="Cancel (⌘.)">
              Stop
            </button>
            <div className="spinner" />
            <span className="muted">{(elapsed / 1000).toFixed(1)} s</span>
          </>
        )}
        <div className="spacer" />
        <span className="faint">{props.driver === "redis" ? "⌘↵ run line · ⇧⌘↵ run all" : "⌘↵ run · ⇧⌘↵ run all · ⌘. stop"}</span>
        <button className={side.open ? "primary" : ""} onClick={side.toggle} title="Query history and saved queries (⌘Y)">
          History
        </button>
      </div>
      <div className="editor-host" style={{ height: results.length ? split.size : undefined, flex: results.length ? undefined : 1 }}>
        <SqlEditor
          driver={props.driver}
          initial={initial.current}
          schema={props.schema}
          defaultSchema={props.defaultSchema}
          onChange={persist}
          onRun={run}
          handle={editor}
        />
      </div>
      {results.length > 0 && (
        <>
          <div className={`hresizer ${split.dragging ? "dragging" : ""}`} onMouseDown={split.onMouseDown} />
          <div className="results">
            <div className="result-tabs">
              {results.map((r, i) => (
                <div
                  key={i}
                  className={`result-tab ${i === current ? "active" : ""} ${r.error ? "err" : ""}`}
                  onClick={() => setCurrent(i)}
                  title={r.sql}
                >
                  {r.error ? "Error" : r.hasRows ? `Result ${i + 1}` : `Statement ${i + 1}`}
                </div>
              ))}
              <div className="spacer" />
              {result && !result.error && (
                <span className="muted" style={{ alignSelf: "center", paddingRight: 6 }}>
                  {result.hasRows
                    ? `${result.rows.length.toLocaleString()} row${result.rows.length === 1 ? "" : "s"}${result.truncated ? " (truncated)" : ""}`
                    : `${result.rowsAffected.toLocaleString()} affected`}{" "}
                  · {result.durationMs} ms{result.notice ? ` · ${result.notice}` : ""}
                </span>
              )}
              {result?.hasRows && !result.error && (
                <button onClick={() => setExporting(true)} title="Export these results to a file">
                  Export…
                </button>
              )}
            </div>
            {result &&
              (result.error ? (
                <div className="message-pane">
                  <div className="error-text">{result.error}</div>
                  <pre>{result.sql}</pre>
                </div>
              ) : result.hasRows ? (
                <div className="grid-wrap">
                  <div className="grid-host">
                    <DataGrid
                      layoutKey={`${current}:${result.sql}`}
                      columns={result.columns.map((c) => ({ name: c.name }))}
                      rowCount={result.rows.length}
                      getValue={(c, r) => ({ value: result.rows[r][c] })}
                      onContextMenu={(c, r, x, y, selected) =>
                        setMenu({
                          x,
                          y,
                          items: [
                            { label: "Copy value", action: () => navigator.clipboard.writeText(result.rows[r][c] ?? "") },
                            {
                              label: "Copy row as JSON",
                              action: () =>
                                navigator.clipboard.writeText(
                                  JSON.stringify(Object.fromEntries(result.columns.map((col, i) => [col.name, result.rows[r][i]])), null, 2),
                                ),
                            },
                            {
                              label: "Copy all as CSV",
                              action: () => navigator.clipboard.writeText(toCsv(result)),
                            },
                            { separator: true, label: "" },
                            ...copyMenuItems(selected.length, () => resultData(result, selected), sqlTarget(result)),
                            { separator: true, label: "" },
                            { label: "Export results…", action: () => setExporting(true) },
                          ],
                        })
                      }
                    />
                  </div>
                </div>
              ) : (
                <div className="message-pane">
                  <div>
                    OK — {result.rowsAffected.toLocaleString()} affected in {result.durationMs} ms
                    {result.notice && <span className="muted"> · {result.notice}</span>}
                  </div>
                  <pre>{result.sql}</pre>
                </div>
              ))}
          </div>
        </>
      )}
      {side.open && (
        <HistoryPanel
          connectionId={props.connectionId}
          editor={editor}
          width={side.size}
          dragging={side.dragging}
          onResize={side.onMouseDown}
          onRunAll={() => editor.current.view && run(editor.current.view, true)}
          onClose={side.close}
        />
      )}
      <ContextMenu menu={menu} onClose={() => setMenu(null)} />
      {confirm && (
        <ConfirmRun
          {...confirm}
          driver={props.driver}
          production={!!props.production}
          onRun={() => (setConfirm(null), execute(confirm.statements))}
          onCancel={() => setConfirm(null)}
        />
      )}
      {exporting && result?.hasRows && (
        <ExportDialog
          title="Export results"
          fileName={guessTable(result.sql)?.replace(/["`[\]]/g, "").split(".").pop() ?? "results"}
          note={result.truncated ? `Only the ${result.rows.length.toLocaleString()} rows shown are exported.` : undefined}
          onExport={async (format, _scope, target) => {
            const data = resultData(result);
            const text =
              format === "csv" ? toDelimited(data, ",") : format === "json" ? toJson(data) : toInserts(data, sqlTarget(result));
            const name = await api.saveTextFile(target, text + "\n");
            return name === null ? null : { name, rows: data.rows.length };
          }}
          onClose={() => setExporting(false)}
        />
      )}
    </div>
  );
}

/** Asks before running writes on production, or UPDATE/DELETE that change every row. */
function ConfirmRun(props: {
  statements: string[];
  unbounded: string[];
  writes: string[];
  driver: DriverKind;
  production: boolean;
  onRun: () => void;
  onCancel: () => void;
}) {
  const plural = (n: number) => (n === 1 ? "1 statement" : `${n} statements`);
  const flagged = props.statements.filter((s) => props.unbounded.includes(s) || props.writes.includes(s));
  return (
    <ConfirmStatements
      title={props.unbounded.length ? "Change every row?" : "Run on production?"}
      statements={flagged.map((s) => (isSql(props.driver) && !s.endsWith(";") ? s + ";" : s))}
      confirmLabel={props.production && props.writes.length ? "Run on production" : "Run anyway"}
      onConfirm={props.onRun}
      onCancel={props.onCancel}
    >
      {props.unbounded.length > 0 && (
        <p style={{ marginTop: 0 }}>
          {plural(props.unbounded.length)} {props.unbounded.length === 1 ? "is an" : "are"} UPDATE or DELETE without a
          WHERE clause, which changes every row in the table.
        </p>
      )}
      {props.production && props.writes.length > 0 && (
        <p style={{ marginTop: 0 }}>
          This is a <b className="error-text">production</b> connection. {plural(props.writes.length)}{" "}
          {props.writes.length === 1 ? "changes" : "change"} data or schema.
        </p>
      )}
      {props.statements.length > flagged.length && (
        <p className="muted" style={{ margin: 0 }}>{plural(props.statements.length)} run in all; the ones below need a look.</p>
      )}
    </ConfirmStatements>
  );
}

function toCsv(result: QueryResult): string {
  const esc = (v: string | null) => (v === null ? "" : /[",\n\r]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  return [result.columns.map((c) => esc(c.name)).join(","), ...result.rows.map((r) => r.map(esc).join(","))].join("\n");
}
