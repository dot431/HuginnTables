import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, Cell, ColumnInfo, DriverKind, errorMessage, FetchRequest, isSql, QueryResult, DeleteImpact } from "../lib/api";
import * as S from "../lib/staging";
import type { TableView } from "../lib/workspaceState";
import { DataGrid } from "./DataGrid";
import { FilterBar, FilterState } from "./FilterBar";
import { ContextMenu, MenuState, Modal, toast } from "./ui";

const PAGE_SIZES = [100, 300, 1000, 5000];

/** Engines with foreign keys, where a delete can leave other rows pointing nowhere. */
const HAS_FOREIGN_KEYS: DriverKind[] = ["postgres", "mysql", "sqlite"];

const RAW_FILTER: Record<DriverKind, { label: string; placeholder: string }> = {
  postgres: { label: "WHERE", placeholder: "price > 10 AND status = 'active'" },
  mysql: { label: "WHERE", placeholder: "price > 10 AND status = 'active'" },
  sqlite: { label: "WHERE", placeholder: "price > 10 AND status = 'active'" },
  bigquery: { label: "WHERE", placeholder: "total > 10 AND customer LIKE 'A%'" },
  redis: { label: "MATCH", placeholder: "user:*:profile" },
  mongodb: { label: "Filter", placeholder: "{ age: { $gt: 30 }, tags: 'vip' }" },
};

/** A query that opens this table in the editor, in the engine's language. */
function openQueryText(driver: DriverKind, schema: string, table: string, result: QueryResult | null): string {
  if (driver === "bigquery") return `SELECT * FROM \`${schema}.${table}\` LIMIT 100;`;
  if (driver === "redis") return `SCAN 0 MATCH ${table} COUNT 100`;
  return (result?.sql ?? `SELECT * FROM ${table} LIMIT 100`) + (driver === "mongodb" ? "" : ";");
}

export function TableTab(props: {
  sessionId: string;
  driver: DriverKind;
  schema: string;
  table: string;
  readOnly: boolean;
  active: boolean;
  onPendingChange: (pending: number) => void;
  onOpenQuery: (sql: string) => void;
  /** Applied on mount; a non-empty filter also opens the filter bar. */
  initialFilter?: FilterState;
  initialSort?: FetchRequest["sort"];
  initialLimit?: number;
  /** Reports filter, sort and page size whenever they change, so the tab can be reopened the same way. */
  onViewChange?: (view: TableView) => void;
}) {
  const { sessionId, schema, table } = props;
  const [view, setView] = useState<"data" | "structure">("data");
  const [structure, setStructure] = useState<ColumnInfo[]>([]);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [limit, setLimit] = useState(props.initialLimit && PAGE_SIZES.includes(props.initialLimit) ? props.initialLimit : 300);
  const [offset, setOffset] = useState(0);
  const [total, setTotal] = useState<number | null>(null);
  const [sort, setSort] = useState<FetchRequest["sort"]>(props.initialSort ?? null);
  const [filter, setFilter] = useState<FilterState>(props.initialFilter ?? { filters: [], rawWhere: "" });
  const [showFilters, setShowFilters] = useState(!!(props.initialFilter?.filters.length || props.initialFilter?.rawWhere));
  const [staging, setStaging] = useState<S.Staging>(S.emptyStaging);
  const [selectedRow, setSelectedRow] = useState<number | undefined>();
  const [showDetail, setShowDetail] = useState(false);
  const [preview, setPreview] = useState<{ sql: string[]; cascade: boolean } | null>(null);
  const [impact, setImpact] = useState<DeleteImpact | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [committing, setCommitting] = useState(false);
  const loadSeq = useRef(0);
  const [loadedSeq, setLoadedSeq] = useState(0);
  const [reveal, setReveal] = useState<{ row: number }>();

  const rows = result?.rows ?? [];
  const columns = useMemo(() => result?.columns.map((c) => c.name) ?? structure.map((c) => c.name), [result, structure]);
  const primaryKey = useMemo(() => structure.filter((c) => c.isPrimaryKey).map((c) => c.name), [structure]);
  const pending = S.pendingCount(staging);
  const editable = !props.readOnly;

  useEffect(() => props.onPendingChange(pending), [pending]);
  useEffect(() => props.onViewChange?.({ filter, sort, limit }), [filter, sort, limit]);

  const request = useCallback(
    (cols: ColumnInfo[]): FetchRequest => ({
      schema, table, limit, offset, filters: filter.filters, rawWhere: filter.rawWhere, sort,
      keyOrder: cols.filter((c) => c.isPrimaryKey).map((c) => c.name),
    }),
    [schema, table, limit, offset, filter, sort],
  );

  const load = useCallback(async (refetchColumns = false) => {
    const seq = ++loadSeq.current;
    setLoading(true);
    setError(null);
    try {
      // Columns come first: the primary key decides the row order.
      const cols = structure.length && !refetchColumns ? structure : await api.tableColumns(sessionId, schema, table);
      const req = request(cols);
      const data = await api.fetchRows(sessionId, req);
      if (seq !== loadSeq.current) return;
      setStructure(cols);
      setResult(data);
      setLoadedSeq(seq);
      setStaging(S.emptyStaging());
      setTotal(null);
      api.countRows(sessionId, req).then((n) => seq === loadSeq.current && setTotal(n)).catch(() => {});
    } catch (e) {
      if (seq === loadSeq.current) setError(errorMessage(e));
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  }, [request, sessionId, schema, table, structure]);

  // Reload whenever paging, sorting or filtering changes.
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [limit, offset, sort, filter]);

  /** Asks before throwing away staged edits. */
  const guard = (then: () => void) => {
    if (pending === 0 || window.confirm(`Discard ${pending} uncommitted change${pending === 1 ? "" : "s"}?`)) then();
  };

  const refresh = () => guard(() => load(true));

  const changeSet = () => S.buildChangeSet(staging, rows, columns, primaryKey, schema, table);

  /**
   * Commits the staged changes. Unless `cascade` is set, deleting rows that
   * other tables still point at stops here and asks what to do.
   */
  const commit = async (cascade = false) => {
    if (!pending || committing) return;
    setCommitting(true);
    try {
      const set = { ...changeSet(), cascade };
      if (!cascade && staging.deleted.size > 0 && HAS_FOREIGN_KEYS.includes(props.driver)) {
        const found = await api.deleteReferences(sessionId, set);
        if (found.references.length) {
          setImpact(found);
          return;
        }
      }
      setImpact(null);
      const affected = await api.commitChanges(sessionId, set);
      toast(`Committed — ${affected} row${affected === 1 ? "" : "s"} affected`, "success");
      setStaging(S.emptyStaging());
      await load();
    } catch (e) {
      toast(`Commit failed, nothing was changed.\n${errorMessage(e)}`, "error");
    } finally {
      setCommitting(false);
    }
  };

  const showPreview = async (cascade = false) => {
    try {
      setPreview({ sql: await api.previewChanges(sessionId, { ...changeSet(), cascade }), cascade });
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const addRow = () => {
    setReveal({ row: rows.length + staging.inserted.length });
    setStaging((s) => S.addRow(s, columns.length));
    setView("data");
  };

  const edit = (col: number, row: number, value: Cell) => setStaging((s) => S.applyEdit(s, rows, col, row, value));

  const getValue = useCallback(
    (col: number, row: number) => S.cellAt(staging, rows, col, row),
    [staging, rows],
  );

  // Keyboard shortcuts while this tab is visible.
  useEffect(() => {
    if (!props.active) return;
    const onKey = (e: KeyboardEvent) => {
      if (!e.metaKey && !e.ctrlKey) return;
      const k = e.key.toLowerCase();
      if (k === "s") {
        e.preventDefault();
        commit();
      } else if (k === "r") {
        e.preventDefault();
        refresh();
      } else if (k === "f") {
        e.preventDefault();
        setShowFilters((v) => !v);
      } else if (k === "i" && editable) {
        e.preventDefault();
        addRow();
      } else if (k === "p" && e.shiftKey) {
        e.preventDefault();
        if (pending) showPreview();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const onContextMenu = (col: number, row: number, x: number, y: number, selected: number[]) => {
    const { value } = S.cellAt(staging, rows, col, row);
    const colName = columns[col];
    const many = selected.length > 1 ? ` (${selected.length})` : "";
    setMenu({
      x,
      y,
      items: [
        { label: "Copy value", action: () => navigator.clipboard.writeText(value ?? "") },
        {
          label: "Copy row as JSON",
          action: () => {
            const obj = Object.fromEntries(columns.map((c, i) => [c, S.cellAt(staging, rows, i, row).value ?? null]));
            navigator.clipboard.writeText(JSON.stringify(obj, null, 2));
          },
        },
        { separator: true, label: "" },
        { label: "Set NULL", disabled: !editable, action: () => edit(col, row, null) },
        { label: "Set empty string", disabled: !editable, action: () => edit(col, row, "") },
        {
          label: `Filter where ${colName} = this`,
          action: () => {
            guard(() => {
              setOffset(0);
              setFilter({
                filters: [value === null || value === undefined ? { column: colName, op: "isNull", value: "" } : { column: colName, op: "=", value }],
                rawWhere: "",
              });
              setShowFilters(true);
            });
          },
        },
        { separator: true, label: "" },
        { label: `Delete row${many}`, shortcut: "⌫", disabled: !editable, action: () => setStaging((s) => S.toggleDelete(s, rows.length, selected)) },
        { label: `Revert changes${many}`, disabled: !pending, action: () => setStaging((s) => S.revertRows(s, rows.length, selected)) },
        { label: "Open row detail", action: () => (setSelectedRow(row), setShowDetail(true)) },
      ],
    });
  };

  const previewText = (lines: string[]) => lines.map((l) => (isSql(props.driver) ? l + ";" : l)).join("\n");

  const from = rows.length ? offset + 1 : 0;
  const to = offset + rows.length;

  return (
    <div className="tab-content">
      <div className="toolbar">
        <div className="segmented">
          <button className={view === "data" ? "on" : ""} onClick={() => setView("data")}>Data</button>
          <button className={view === "structure" ? "on" : ""} onClick={() => setView("structure")}>Structure</button>
        </div>
        <button className={showFilters || filter.filters.length || filter.rawWhere ? "primary" : ""} onClick={() => setShowFilters((v) => !v)} title="Filter rows (⌘F)">
          Filter{filter.filters.length || filter.rawWhere ? ` (${filter.filters.length || 1})` : ""}
        </button>
        <button onClick={refresh} title="Reload (⌘R)">Reload</button>
        {editable && <button onClick={addRow} title="Insert row (⌘I)">+ Row</button>}
        <button
          onClick={() => props.onOpenQuery(openQueryText(props.driver, schema, table, result))}
          title="Open this table in a query editor tab"
        >
          Query
        </button>
        <div className="spacer" />
        {props.readOnly && <span className="faint">Read-only</span>}
        {editable && !primaryKey.length && structure.length > 0 && props.driver !== "mongodb" && (
          <span className="faint" title="Rows are matched on every column; only one matching row is changed per edit.">
            No primary key
          </span>
        )}
        <button className={showDetail ? "primary" : ""} onClick={() => setShowDetail((v) => !v)} title="Row detail panel">
          Detail
        </button>
      </div>

      {showFilters && view === "data" && columns.length > 0 && (
        <FilterBar
          columns={columns}
          initial={filter}
          raw={RAW_FILTER[props.driver]}
          onApply={(f) =>
            guard(() => {
              setOffset(0);
              setFilter(f);
            })
          }
        />
      )}

      {view === "structure" ? (
        <StructureView columns={structure} />
      ) : (
        <div className="grid-wrap">
          <div className="grid-host">
            {error ? (
              <div className="message-pane error-text">{error}</div>
            ) : (
              <DataGrid
                layoutKey={`${schema}.${table}`}
                selectionKey={loadedSeq}
                reveal={reveal}
                columns={columns.map((name) => ({ name, badge: primaryKey.includes(name) ? "🔑" : undefined }))}
                rowCount={rows.length + staging.inserted.length}
                getValue={getValue}
                editable={editable}
                sort={sort}
                onEdit={(edits) =>
                  setStaging((s) => edits.reduce((acc, e) => {
                    const current = S.cellAt(acc, rows, e.col, e.row).value;
                    // Opening the editor on NULL and leaving it empty shouldn't turn it into ''.
                    if ((current === null || current === undefined) && e.value === "") return acc;
                    return S.applyEdit(acc, rows, e.col, e.row, e.value);
                  }, s))
                }
                onDeleteRows={(rs) => setStaging((s) => S.toggleDelete(s, rows.length, rs))}
                onHeaderClick={(col) =>
                  guard(() => {
                    const name = columns[col];
                    setSort((cur) =>
                      cur?.column !== name ? { column: name, desc: false } : !cur.desc ? { column: name, desc: true } : null,
                    );
                  })
                }
                onContextMenu={onContextMenu}
                onSelectRow={setSelectedRow}
              />
            )}
            {loading && (
              <div className="grid-overlay">
                <div className="spinner" />
              </div>
            )}
          </div>
          {showDetail && (
            <RowDetail
              columns={structure.length ? structure : columns.map((name) => ({ name, dataType: "", nullable: true, default: null, isPrimaryKey: false }))}
              row={selectedRow}
              getValue={(c) => (selectedRow === undefined ? { value: undefined, state: undefined } : S.cellAt(staging, rows, c, selectedRow))}
              editable={editable}
              onEdit={(c, v) => selectedRow !== undefined && edit(c, selectedRow, v)}
              onClose={() => setShowDetail(false)}
            />
          )}
        </div>
      )}

      <div className="footer">
        {pending > 0 ? (
          <>
            <span className="pending-badge">{pending} pending</span>
            <button onClick={() => setStaging(S.emptyStaging())}>Discard</button>
            <button onClick={() => showPreview()} title="Preview SQL (⇧⌘P)">Preview SQL</button>
            <button className="primary" onClick={() => commit()} disabled={committing} title="Commit (⌘S)">
              {committing ? "Committing…" : "Commit"}
            </button>
          </>
        ) : (
          <span className="muted">
            {result ? `${result.durationMs} ms` : ""}
            {result?.notice ? ` · ${result.notice}` : ""}
          </span>
        )}
        <div className="spacer" />
        <span className="muted">
          {from}–{to} of {total === null ? "…" : total.toLocaleString()}
        </span>
        <button className="icon" disabled={offset === 0} onClick={() => guard(() => setOffset(Math.max(0, offset - limit)))}>
          ‹
        </button>
        <button
          className="icon"
          disabled={total !== null ? to >= total : rows.length < limit}
          onClick={() => guard(() => setOffset(offset + limit))}
        >
          ›
        </button>
        <select value={limit} onChange={(e) => guard(() => (setOffset(0), setLimit(Number(e.target.value))))}>
          {PAGE_SIZES.map((n) => (
            <option key={n} value={n}>{n} rows</option>
          ))}
        </select>
      </div>

      <ContextMenu menu={menu} onClose={() => setMenu(null)} />
      {preview && (
        <Modal
          title="Pending changes"
          wide
          onClose={() => setPreview(null)}
          footer={
            <>
              <span className="muted">
                {props.driver === "redis"
                  ? "Runs atomically (MULTI/EXEC)."
                  : props.driver === "mongodb"
                    ? "Runs one change at a time; MongoDB doesn't roll back earlier ones."
                    : "Runs in a single transaction."}
              </span>
              <div className="spacer" />
              <button onClick={() => navigator.clipboard.writeText(previewText(preview.sql))}>Copy</button>
              <button onClick={() => setPreview(null)}>Close</button>
              <button className="primary" onClick={() => (setPreview(null), commit(preview.cascade))}>Commit</button>
            </>
          }
        >
          <pre className="sql-preview">{previewText(preview.sql)}</pre>
        </Modal>
      )}
      {impact && (
        <DeleteReferences
          table={table}
          deleting={staging.deleted.size}
          impact={impact}
          committing={committing}
          onStop={() => (setImpact(null), setStaging(S.clearDeletes))}
          onPreview={() => (setImpact(null), showPreview(true))}
          onCascade={() => commit(true)}
          onClose={() => setImpact(null)}
        />
      )}
    </div>
  );
}

/** Asks whether to keep the rows or delete everything that points at them too. */
function DeleteReferences(props: {
  table: string;
  deleting: number;
  impact: DeleteImpact;
  committing: boolean;
  onStop: () => void;
  onPreview: () => void;
  onCascade: () => void;
  onClose: () => void;
}) {
  const { references, cascadedRows } = props.impact;
  const cleared = references.some((r) => r.onDelete.startsWith("SET "));
  const plural = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;
  const effect = (onDelete: string) =>
    onDelete === "SET NULL" ? "cleared (NULL)" : onDelete === "SET DEFAULT" ? "reset to default" : "deleted";
  return (
    <Modal
      title={`${props.deleting === 1 ? "This row is" : "These rows are"} still referenced`}
      wide
      onClose={props.onClose}
      footer={
        <>
          <button onClick={props.onPreview}>Preview SQL</button>
          <div className="spacer" />
          <button onClick={props.onStop}>Stop deleting</button>
          <button className="primary danger-fill" onClick={props.onCascade} disabled={props.committing}>
            {props.committing ? "Deleting…" : `Delete all ${plural(props.deleting + cascadedRows, "row")}`}
          </button>
        </>
      }
    >
      <p style={{ marginTop: 0 }}>
        Other tables point at the {plural(props.deleting, "row")} you're deleting from <b>{props.table}</b>.
        {cascadedRows > 0 && <> Deleting {props.deleting === 1 ? "it" : "them"} also deletes {plural(cascadedRows, "row")} in those tables.</>}
      </p>
      <table className="plain">
        <thead>
          <tr>
            <th>Table</th>
            <th>Points at</th>
            <th>On delete</th>
            <th>Rows will be</th>
            <th style={{ textAlign: "right" }}>Rows</th>
          </tr>
        </thead>
        <tbody>
          {references.map((r, i) => (
            <tr key={i}>
              <td style={{ paddingLeft: 10 + (r.depth - 1) * 18 }}>
                {r.depth > 1 && <span className="faint">↳ </span>}
                <b>{r.table}</b>
              </td>
              <td className="mono muted">{r.columns.join(", ")} → {r.parentTable}</td>
              <td className="mono muted">{r.onDelete}</td>
              <td className={r.onDelete.startsWith("SET ") ? "muted" : "error-text"}>{effect(r.onDelete)}</td>
              <td style={{ textAlign: "right" }}>{r.rows.toLocaleString()}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="muted" style={{ marginBottom: 0 }}>
        <b>Stop deleting</b> keeps every row and unmarks the deletion; your other changes stay staged.{" "}
        <b>Delete all</b> deletes the rows marked “deleted”, deepest first, in one transaction
        {cleared ? "; rows marked “cleared” stay, and the database empties their reference." : "."}
      </p>
    </Modal>
  );
}

function StructureView({ columns }: { columns: ColumnInfo[] }) {
  return (
    <div className="structure">
      <table className="plain">
        <thead>
          <tr>
            <th style={{ width: 40 }}>#</th>
            <th>Column</th>
            <th>Type</th>
            <th>Nullable</th>
            <th>Default</th>
            <th>Key</th>
          </tr>
        </thead>
        <tbody>
          {columns.map((c, i) => (
            <tr key={c.name}>
              <td className="faint">{i + 1}</td>
              <td style={{ fontWeight: 500 }}>{c.name}</td>
              <td className="mono">{c.dataType}</td>
              <td>{c.nullable ? "YES" : "NO"}</td>
              <td className="mono muted">{c.default ?? <span className="faint">NULL</span>}</td>
              <td>{c.isPrimaryKey && <span className="pk">PRIMARY</span>}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RowDetail(props: {
  columns: ColumnInfo[];
  row: number | undefined;
  getValue: (col: number) => { value: Cell | undefined; state: S.CellState };
  editable: boolean;
  onEdit: (col: number, value: Cell) => void;
  onClose: () => void;
}) {
  return (
    <div className="detail-panel">
      <div className="dp-head">
        {props.row === undefined ? "No row selected" : `Row ${props.row + 1}`}
        <div className="spacer" />
        <button className="ghost icon" onClick={props.onClose}>×</button>
      </div>
      {props.row !== undefined && (
        <div className="dp-body">
          {props.columns.map((c, i) => {
            const { value, state } = props.getValue(i);
            const readOnly = !props.editable || state === "deleted";
            return (
              <div key={c.name} className={`dp-field ${state === "edited" ? "edited" : ""}`}>
                <label>
                  <span>
                    {c.isPrimaryKey && "🔑 "}
                    <b style={{ color: "var(--text)", fontWeight: 600 }}>{c.name}</b> {c.dataType}
                  </span>
                  {!readOnly && c.nullable && value !== null && (
                    <span className="null-tag" style={{ cursor: "pointer" }} onClick={() => props.onEdit(i, null)}>
                      set NULL
                    </span>
                  )}
                </label>
                <textarea
                  rows={Math.min(6, Math.max(1, (value ?? "").split("\n").length))}
                  readOnly={readOnly}
                  value={value ?? ""}
                  placeholder={value === null ? "NULL" : value === undefined ? "DEFAULT" : ""}
                  onChange={(e) => props.onEdit(i, e.target.value)}
                />
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
