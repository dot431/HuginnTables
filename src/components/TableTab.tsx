import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, Cell, ColumnInfo, errorMessage, FetchRequest, QueryResult } from "../lib/api";
import * as S from "../lib/staging";
import { DataGrid } from "./DataGrid";
import { FilterBar, FilterState } from "./FilterBar";
import { ContextMenu, MenuState, Modal, toast } from "./ui";

const PAGE_SIZES = [100, 300, 1000, 5000];

export function TableTab(props: {
  sessionId: string;
  schema: string;
  table: string;
  readOnly: boolean;
  active: boolean;
  onPendingChange: (pending: number) => void;
  onOpenQuery: (sql: string) => void;
}) {
  const { sessionId, schema, table } = props;
  const [view, setView] = useState<"data" | "structure">("data");
  const [structure, setStructure] = useState<ColumnInfo[]>([]);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [limit, setLimit] = useState(300);
  const [offset, setOffset] = useState(0);
  const [total, setTotal] = useState<number | null>(null);
  const [sort, setSort] = useState<FetchRequest["sort"]>(null);
  const [filter, setFilter] = useState<FilterState>({ filters: [], rawWhere: "" });
  const [showFilters, setShowFilters] = useState(false);
  const [staging, setStaging] = useState<S.Staging>(S.emptyStaging);
  const [selectedRow, setSelectedRow] = useState<number | undefined>();
  const [showDetail, setShowDetail] = useState(false);
  const [preview, setPreview] = useState<string[] | null>(null);
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

  const commit = async () => {
    if (!pending || committing) return;
    setCommitting(true);
    try {
      const affected = await api.commitChanges(sessionId, changeSet());
      toast(`Committed — ${affected} row${affected === 1 ? "" : "s"} affected`, "success");
      setStaging(S.emptyStaging());
      await load();
    } catch (e) {
      toast(`Commit failed, nothing was changed.\n${errorMessage(e)}`, "error");
    } finally {
      setCommitting(false);
    }
  };

  const showPreview = async () => {
    try {
      setPreview(await api.previewChanges(sessionId, changeSet()));
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
          onClick={() => props.onOpenQuery(result?.sql ?? `SELECT * FROM ${table} LIMIT 100`)}
          title="Open this query in a SQL editor tab"
        >
          SQL
        </button>
        <div className="spacer" />
        {props.readOnly && <span className="faint">Read-only view</span>}
        {editable && !primaryKey.length && structure.length > 0 && (
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
            <button onClick={showPreview} title="Preview SQL (⇧⌘P)">Preview SQL</button>
            <button className="primary" onClick={commit} disabled={committing} title="Commit (⌘S)">
              {committing ? "Committing…" : "Commit"}
            </button>
          </>
        ) : (
          <span className="muted">
            {result ? `${result.durationMs} ms` : ""}
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
              <span className="muted">Runs in a single transaction.</span>
              <div className="spacer" />
              <button onClick={() => navigator.clipboard.writeText(preview.map((s) => s + ";").join("\n"))}>Copy</button>
              <button onClick={() => setPreview(null)}>Close</button>
              <button className="primary" onClick={() => (setPreview(null), commit())}>Commit</button>
            </>
          }
        >
          <pre className="sql-preview">{preview.map((s) => s + ";").join("\n")}</pre>
        </Modal>
      )}
    </div>
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
