import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, Cell, ColumnInfo, DriverKind, errorMessage, FetchRequest, isSql, QueryResult, DeleteImpact, ForeignKeyInfo, TableForeignKeys } from "../lib/api";
import { pastedRows } from "../lib/formats";
import * as S from "../lib/staging";
import { copyMenuItems, ExportDialog, PasteDialog } from "./CopyExport";
import type { TableView } from "../lib/workspaceState";
import { DataGrid } from "./DataGrid";
import { FilterBar, FilterState } from "./FilterBar";
import { keyFilter, openParentLabel, ReferencedBy } from "./ForeignKeyNav";
import { ConfirmStatements } from "./Safety";
import { StructureView } from "./StructureView";
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
  /** Commits show their statements and ask first. */
  production?: boolean;
  active: boolean;
  onPendingChange: (pending: number) => void;
  onOpenQuery: (sql: string) => void;
  /** Applied on mount; a non-empty filter also opens the filter bar. */
  initialFilter?: FilterState;
  initialSort?: FetchRequest["sort"];
  initialLimit?: number;
  /** Reports filter, sort and page size whenever they change, so the tab can be reopened the same way. */
  onViewChange?: (view: TableView) => void;
  /** Opens a table in a new tab, showing only the rows `filter` selects. */
  onOpenTable?: (schema: string, table: string, filter: FilterState) => void;
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
  const [confirmProd, setConfirmProd] = useState<{ sql: string[]; cascade: boolean } | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [committing, setCommitting] = useState(false);
  const loadSeq = useRef(0);
  const [loadedSeq, setLoadedSeq] = useState(0);
  const [reveal, setReveal] = useState<{ row: number }>();
  const [foreignKeys, setForeignKeys] = useState<TableForeignKeys>({ outgoing: [], incoming: [] });
  const [referencedBy, setReferencedBy] = useState<number | null>(null);
  const [exporting, setExporting] = useState(false);
  const [pasteBox, setPasteBox] = useState(false);

  const rows = result?.rows ?? [];
  const columns = useMemo(() => result?.columns.map((c) => c.name) ?? structure.map((c) => c.name), [result, structure]);
  const primaryKey = useMemo(() => structure.filter((c) => c.isPrimaryKey).map((c) => c.name), [structure]);
  const pending = S.pendingCount(staging);
  const editable = !props.readOnly;
  const navigable = !!props.onOpenTable && HAS_FOREIGN_KEYS.includes(props.driver);
  const fkColumns = useMemo(() => new Set(foreignKeys.outgoing.flatMap((fk) => fk.columns)), [foreignKeys]);

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

  const loadForeignKeys = useCallback(() => {
    if (!navigable) return;
    api.tableForeignKeys(sessionId, schema, table).then(setForeignKeys).catch(() => {});
  }, [navigable, sessionId, schema, table]);

  useEffect(() => loadForeignKeys(), [loadForeignKeys]);

  // Reload whenever paging, sorting or filtering changes.
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [limit, offset, sort, filter]);

  /** Asks before throwing away staged edits. */
  const guard = (then: () => void) => {
    if (pending === 0 || window.confirm(`Discard ${pending} uncommitted change${pending === 1 ? "" : "s"}?`)) then();
  };

  const refresh = () => guard(() => (load(true), loadForeignKeys()));

  const changeSet = () => S.buildChangeSet(staging, rows, columns, primaryKey, schema, table);

  /**
   * Commits the staged changes. Unless `cascade` is set, deleting rows that
   * other tables still point at stops here and asks what to do. On production
   * the statements are shown for confirmation first, unless `confirmed`.
   */
  const commit = async (cascade = false, confirmed = false) => {
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
      if (props.production && !confirmed) {
        setConfirmProd({ sql: await api.previewChanges(sessionId, set), cascade });
        return;
      }
      setConfirmProd(null);
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

  /** Stages spreadsheet or CSV text as new rows. */
  const pasteRows = (text: string) => {
    const { rows: added, byName } = pastedRows(text, columns);
    if (!added.length) {
      toast("Nothing to paste: the clipboard has no rows");
      return;
    }
    setReveal({ row: rows.length + staging.inserted.length });
    setStaging((s) => S.addRows(s, added));
    setView("data");
    const n = added.length;
    toast(`Staged ${n} new row${n === 1 ? "" : "s"}, matched ${byName ? "by column name" : "by position"}`, "success");
  };

  const pasteFromClipboard = async () => {
    if (!editable || !columns.length) return;
    let text: string;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      setPasteBox(true);
      return;
    }
    pasteRows(text);
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
      } else if (k === "v" && e.shiftKey && editable) {
        // Leave text fields their own paste.
        const el = e.target as HTMLElement | null;
        if (el?.closest("input, textarea, [contenteditable=true]")) return;
        e.preventDefault();
        pasteFromClipboard();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  /** The values of `columns` in a row, as currently staged. */
  const rowValues = (row: number, cols: string[]) =>
    cols.map((c) => (columns.includes(c) ? S.cellAt(staging, rows, columns.indexOf(c), row).value : undefined));

  /** Opens the row that `fk` points at from `row`, if every key column has a value. */
  const openParent = (fk: ForeignKeyInfo, row: number) => {
    const f = keyFilter(fk.parentColumns, rowValues(row, fk.columns));
    if (f) props.onOpenTable?.(fk.parentSchema, fk.parentTable, f);
  };

  const onMetaClick = (col: number, row: number) => {
    const fk = foreignKeys.outgoing.find((k) => k.columns.includes(columns[col]));
    if (fk) openParent(fk, row);
  };

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
        ...copyMenuItems(
          selected.length,
          () => ({ columns, rows: selected.map((r) => columns.map((_, c) => S.cellAt(staging, rows, c, r).value)) }),
          { driver: props.driver, schema, table, primaryKey },
          true,
        ),
        { label: "Paste rows from clipboard", shortcut: "⇧⌘V", disabled: !editable, action: pasteFromClipboard },
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
        ...(navigable
          ? [
              { separator: true, label: "" },
              ...foreignKeys.outgoing
                .filter((fk) => fk.columns.includes(colName))
                .map((fk) => {
                  const values = rowValues(row, fk.columns);
                  return {
                    label: openParentLabel(fk, values),
                    shortcut: "⌘-click",
                    disabled: !keyFilter(fk.parentColumns, values),
                    action: () => openParent(fk, row),
                  };
                }),
              { label: "Referenced by…", disabled: !foreignKeys.incoming.length || row >= rows.length, action: () => setReferencedBy(row) },
            ]
          : []),
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
        <button onClick={() => setExporting(true)} disabled={!columns.length} title="Export rows to a file">
          Export…
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
        <StructureView
          sessionId={sessionId}
          driver={props.driver}
          schema={schema}
          table={table}
          columns={structure}
          readOnly={props.readOnly}
          pending={pending}
          reloadKey={loadedSeq}
          onChanged={() => load(true)}
        />
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
                columns={columns.map((name) => ({
                  name,
                  badge: (primaryKey.includes(name) ? "🔑" : "") + (fkColumns.has(name) ? "↗" : "") || undefined,
                }))}
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
                onCellMetaClick={navigable ? onMetaClick : undefined}
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
      {exporting && (
        <ExportDialog
          title={`Export ${table}`}
          fileName={table}
          scopes={[
            { id: "view", label: "Current filter & sort" },
            { id: "all", label: "Whole table" },
          ]}
          initialScope={filter.filters.length || filter.rawWhere || sort ? "view" : "all"}
          note={pending ? "Exports what's in the database; uncommitted changes are left out." : undefined}
          onExport={(format, scope, path) =>
            api.exportTable(
              sessionId,
              scope === "view"
                ? { schema, table, filters: filter.filters, rawWhere: filter.rawWhere, sort, keyOrder: primaryKey }
                : { schema, table, filters: [], rawWhere: "", sort: null, keyOrder: primaryKey },
              format,
              path,
            )
          }
          onClose={() => setExporting(false)}
        />
      )}
      {pasteBox && <PasteDialog onPaste={pasteRows} onClose={() => setPasteBox(false)} />}
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
              {props.production ? (
                <button className="primary danger-fill" onClick={() => (setPreview(null), commit(preview.cascade, true))}>
                  Commit to production
                </button>
              ) : (
                <button className="primary" onClick={() => (setPreview(null), commit(preview.cascade))}>Commit</button>
              )}
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
      {confirmProd && (
        <ConfirmStatements
          title="Commit to production?"
          statements={[previewText(confirmProd.sql)]}
          confirmLabel={`Commit ${pending} change${pending === 1 ? "" : "s"}`}
          busy={committing}
          onConfirm={() => commit(confirmProd.cascade, true)}
          onCancel={() => setConfirmProd(null)}
        >
          <b>{table}</b> is on a production connection. These statements run{" "}
          {props.driver === "redis" ? "atomically" : props.driver === "mongodb" ? "one at a time" : "in a single transaction"}:
        </ConfirmStatements>
      )}
      {referencedBy !== null && (
        <ReferencedBy
          sessionId={sessionId}
          table={table}
          incoming={foreignKeys.incoming}
          // References point at the stored values, not staged edits.
          valueOf={(c) => (columns.includes(c) ? rows[referencedBy]?.[columns.indexOf(c)] : undefined)}
          onOpen={(fk, f) => (setReferencedBy(null), props.onOpenTable?.(fk.schema, fk.table, f))}
          onClose={() => setReferencedBy(null)}
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
