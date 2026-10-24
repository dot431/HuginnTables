import { ReactNode, useCallback, useEffect, useState } from "react";
import { api, ColumnInfo, DriverKind, errorMessage, ForeignKeyInfo, IndexInfo, SchemaChange, TableForeignKeys } from "../lib/api";
import { Modal, toast } from "./ui";

/** Engines whose indexes, DDL and schema changes the Structure tab shows. */
const SCHEMA_TOOLS: DriverKind[] = ["postgres", "mysql", "sqlite"];

/** Common column types, offered while typing one. */
const TYPE_SUGGESTIONS: Partial<Record<DriverKind, string[]>> = {
  postgres: [
    "integer", "bigint", "smallint", "serial", "bigserial", "numeric(10,2)", "real", "double precision", "boolean",
    "text", "varchar(255)", "char(1)", "uuid", "date", "time", "timestamp", "timestamptz", "interval", "json", "jsonb",
    "bytea", "inet", "text[]",
  ],
  mysql: [
    "INT", "BIGINT", "SMALLINT", "TINYINT(1)", "DECIMAL(10,2)", "FLOAT", "DOUBLE", "VARCHAR(255)", "CHAR(1)", "TEXT",
    "MEDIUMTEXT", "LONGTEXT", "DATE", "DATETIME", "TIMESTAMP", "TIME", "YEAR", "JSON", "BLOB", "ENUM('a','b')",
  ],
  sqlite: ["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC", "BOOLEAN", "DATE", "DATETIME"],
};

/** Longest identifier Postgres keeps (MySQL allows 64). */
const MAX_NAME = 63;

/** One foreign key: this table's own (outgoing) or another table's pointing here (incoming). */
function ForeignKeyRow({ fk, schema, incoming }: { fk: ForeignKeyInfo; schema: string; incoming: boolean }) {
  const name = (s: string, t: string) => (s && s !== schema ? `${s}.${t}` : t);
  return (
    <tr>
      <td style={{ fontWeight: 500 }}>{fk.name}</td>
      <td className="muted">{incoming ? "← from " + name(fk.schema, fk.table) : "→ out"}</td>
      <td className="mono">{fk.columns.join(", ")}</td>
      <td className="mono">
        {name(fk.parentSchema, fk.parentTable)} ({fk.parentColumns.join(", ")})
      </td>
      <td className="mono muted">{fk.onDelete}</td>
    </tr>
  );
}

type Dialog = { kind: "addColumn" } | { kind: "addIndex" } | { kind: "dropIndex"; index: IndexInfo };

export function StructureView(props: {
  sessionId: string;
  driver: DriverKind;
  schema: string;
  table: string;
  columns: ColumnInfo[];
  /** Hides the actions that change the schema. */
  readOnly: boolean;
  /** Uncommitted grid edits; reloading the table after a change discards them. */
  pending: number;
  /** Changes whenever the table tab reloads, so indexes and DDL follow. */
  reloadKey: number;
  /** Called after a schema change ran, to reload the table's columns and rows. */
  onChanged: () => void;
}) {
  const { sessionId, schema, table } = props;
  const tools = SCHEMA_TOOLS.includes(props.driver);
  const canChange = tools && !props.readOnly;
  const [indexes, setIndexes] = useState<IndexInfo[] | null>(null);
  const [indexError, setIndexError] = useState<string | null>(null);
  const [ddl, setDdl] = useState<string | null>(null);
  const [ddlError, setDdlError] = useState<string | null>(null);
  const [foreignKeys, setForeignKeys] = useState<TableForeignKeys | null>(null);
  const [fkError, setFkError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);

  const load = useCallback(() => {
    if (!tools) return;
    api.tableIndexes(sessionId, schema, table).then(
      (ix) => (setIndexes(ix), setIndexError(null)),
      (e) => setIndexError(errorMessage(e)),
    );
    api.tableForeignKeys(sessionId, schema, table).then(
      (fks) => (setForeignKeys(fks), setFkError(null)),
      (e) => setFkError(errorMessage(e)),
    );
    api.tableDdl(sessionId, schema, table).then(
      (text) => (setDdl(text), setDdlError(null)),
      (e) => setDdlError(errorMessage(e)),
    );
  }, [tools, sessionId, schema, table]);

  useEffect(load, [load, props.reloadKey]);

  const changed = () => {
    setDialog(null);
    load();
    props.onChanged();
  };

  const common = { sessionId, schema, table, pending: props.pending, onClose: () => setDialog(null), onDone: changed };
  const showDefinition = indexes?.some((i) => i.definition);

  return (
    <div className="structure">
      <section className="structure-section">
        <div className="structure-head">
          <h3>Columns</h3>
          <span className="faint">{props.columns.length}</span>
          <div className="spacer" />
          {canChange && <button onClick={() => setDialog({ kind: "addColumn" })}>+ Column</button>}
        </div>
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
            {props.columns.map((c, i) => (
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
      </section>

      {tools && (
        <section className="structure-section">
          <div className="structure-head">
            <h3>Indexes</h3>
            {indexes && <span className="faint">{indexes.length}</span>}
            <div className="spacer" />
            {canChange && (
              <button onClick={() => setDialog({ kind: "addIndex" })} disabled={!props.columns.length}>
                + Index
              </button>
            )}
          </div>
          {indexError ? (
            <div className="structure-note error-text">{indexError}</div>
          ) : !indexes ? (
            <div className="structure-note faint">Loading…</div>
          ) : !indexes.length ? (
            <div className="structure-note faint">No indexes</div>
          ) : (
            <table className="plain">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Columns</th>
                  <th>Type</th>
                  <th>Kind</th>
                  {showDefinition && <th>Definition</th>}
                  {canChange && <th />}
                </tr>
              </thead>
              <tbody>
                {indexes.map((ix) => (
                  <tr key={ix.name}>
                    <td style={{ fontWeight: 500 }}>{ix.name}</td>
                    <td className="mono">{ix.columns.join(", ")}</td>
                    <td className="mono muted">{ix.method ?? ""}</td>
                    <td>
                      {ix.primary ? (
                        <span className="pk">PRIMARY</span>
                      ) : ix.unique ? (
                        <span className="unique-tag">UNIQUE</span>
                      ) : (
                        <span className="faint">INDEX</span>
                      )}
                    </td>
                    {showDefinition && <td className="mono muted">{ix.definition ?? ""}</td>}
                    {canChange && (
                      <td style={{ textAlign: "right" }}>
                        {/* SQLite keeps PRIMARY KEY/UNIQUE indexes in the table definition. */}
                        {!(props.driver === "sqlite" && ix.constraint) && (
                          <button className="ghost danger" onClick={() => setDialog({ kind: "dropIndex", index: ix })}>
                            Drop
                          </button>
                        )}
                      </td>
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      )}

      {tools && (
        <section className="structure-section">
          <div className="structure-head">
            <h3>Foreign keys</h3>
            {foreignKeys && <span className="faint">{foreignKeys.outgoing.length + foreignKeys.incoming.length}</span>}
          </div>
          {fkError ? (
            <div className="structure-note error-text">{fkError}</div>
          ) : !foreignKeys ? (
            <div className="structure-note faint">Loading…</div>
          ) : !foreignKeys.outgoing.length && !foreignKeys.incoming.length ? (
            <div className="structure-note faint">No foreign keys</div>
          ) : (
            <table className="plain">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Direction</th>
                  <th>Columns</th>
                  <th>References</th>
                  <th>On delete</th>
                </tr>
              </thead>
              <tbody>
                {foreignKeys.outgoing.map((fk) => <ForeignKeyRow key={`o:${fk.name}`} fk={fk} schema={schema} incoming={false} />)}
                {foreignKeys.incoming.map((fk) => (
                  <ForeignKeyRow key={`i:${fk.schema}.${fk.table}.${fk.name}`} fk={fk} schema={schema} incoming />
                ))}
              </tbody>
            </table>
          )}
        </section>
      )}

      {tools && (
        <section className="structure-section">
          <div className="structure-head">
            <h3>DDL</h3>
            <div className="spacer" />
            <button
              disabled={!ddl}
              onClick={() => ddl && navigator.clipboard.writeText(ddl).then(() => toast("DDL copied", "success"))}
            >
              Copy
            </button>
          </div>
          {ddlError ? (
            <div className="structure-note error-text">{ddlError}</div>
          ) : ddl === null ? (
            <div className="structure-note faint">Loading…</div>
          ) : (
            <pre className="sql-preview ddl">{ddl}</pre>
          )}
        </section>
      )}

      {dialog?.kind === "addColumn" && <AddColumnDialog {...common} driver={props.driver} />}
      {dialog?.kind === "addIndex" && <AddIndexDialog {...common} columns={props.columns} />}
      {dialog?.kind === "dropIndex" && (
        <ChangeDialog
          {...common}
          title={`Drop index ${dialog.index.name}?`}
          change={{ kind: "dropIndex", name: dialog.index.name, constraint: dialog.index.constraint }}
          confirm="Drop index"
          danger
        >
          <p style={{ marginTop: 0 }}>
            {dialog.index.primary
              ? "This removes the table's primary key."
              : dialog.index.constraint
                ? "This index backs a UNIQUE constraint; the constraint is dropped with it."
                : "The index is removed; the data in the table stays."}
          </p>
        </ChangeDialog>
      )}
    </div>
  );
}

type CommonProps = {
  sessionId: string;
  schema: string;
  table: string;
  pending: number;
  onClose: () => void;
  onDone: () => void;
};

/**
 * Shows the SQL for `change` as the form above it is filled in, and runs it on
 * confirm. The SQL comes from the backend so quoting matches the engine.
 */
function ChangeDialog(
  props: CommonProps & { title: string; change: SchemaChange; confirm: string; danger?: boolean; children?: ReactNode },
) {
  const { sessionId, schema, table } = props;
  const [sql, setSql] = useState<string[]>([]);
  const [hint, setHint] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const key = JSON.stringify(props.change);

  useEffect(() => {
    let live = true;
    setError(null);
    api.schemaChangeSql(sessionId, schema, table, props.change).then(
      (s) => live && (setSql(s), setHint(null)),
      (e) => live && (setSql([]), setHint(errorMessage(e))),
    );
    return () => void (live = false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, sessionId, schema, table]);

  const text = sql.map((s) => s + ";").join("\n");

  const run = async () => {
    if (!sql.length || running) return;
    setRunning(true);
    setError(null);
    try {
      const failed = (await api.runQuery(sessionId, sql)).find((r) => r.error);
      if (failed) {
        setError(failed.error!);
        return;
      }
      toast(`${props.confirm} — done`, "success");
      props.onDone();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <Modal
      title={props.title}
      wide
      onClose={props.onClose}
      footer={
        <>
          {props.pending > 0 && (
            <span className="muted">
              Discards {props.pending} uncommitted change{props.pending === 1 ? "" : "s"} in this tab.
            </span>
          )}
          <div className="spacer" />
          <button disabled={!sql.length} onClick={() => navigator.clipboard.writeText(text)}>Copy</button>
          <button onClick={props.onClose}>Cancel</button>
          <button
            className={props.danger ? "primary danger-fill" : "primary"}
            disabled={!sql.length || running}
            onClick={run}
          >
            {running ? "Running…" : props.confirm}
          </button>
        </>
      }
    >
      {props.children}
      <div className="form-section">SQL</div>
      {sql.length ? <pre className="sql-preview">{text}</pre> : <div className="faint">{hint ?? "…"}</div>}
      {error && <div className="notice err" style={{ marginTop: 10 }}>{error}</div>}
    </Modal>
  );
}

function AddColumnDialog(props: CommonProps & { driver: DriverKind }) {
  const [name, setName] = useState("");
  const [type, setType] = useState("");
  const [nullable, setNullable] = useState(true);
  const [def, setDef] = useState("");
  const listId = `types-${props.driver}`;
  return (
    <ChangeDialog
      {...props}
      title={`Add column to ${props.table}`}
      change={{ kind: "addColumn", name, dataType: type, nullable, default: def.trim() ? def : null }}
      confirm="Add column"
    >
      <div className="form-grid">
        <label>Name</label>
        <input value={name} autoFocus spellCheck={false} onChange={(e) => setName(e.target.value)} />
        <label>Type</label>
        <input list={listId} value={type} spellCheck={false} placeholder="Pick or type a type" onChange={(e) => setType(e.target.value)} />
        <datalist id={listId}>
          {(TYPE_SUGGESTIONS[props.driver] ?? []).map((t) => <option key={t} value={t} />)}
        </datalist>
        <label>Default</label>
        <input
          className="mono"
          value={def}
          spellCheck={false}
          placeholder="None — an SQL expression: 0, 'text', now()"
          onChange={(e) => setDef(e.target.value)}
        />
        <span />
        <label className="check">
          <input type="checkbox" checked={nullable} onChange={(e) => setNullable(e.target.checked)} />
          Allow NULL
        </label>
      </div>
    </ChangeDialog>
  );
}

function AddIndexDialog(props: CommonProps & { columns: ColumnInfo[] }) {
  const [selected, setSelected] = useState<string[]>([]);
  const [unique, setUnique] = useState(false);
  const [name, setName] = useState<string | null>(null);
  // Follows the columns until the name is edited by hand.
  const suggested = selected.length
    ? `${props.table}_${selected.join("_")}_${unique ? "key" : "idx"}`.slice(0, MAX_NAME)
    : "";
  const toggle = (col: string) => setSelected((s) => (s.includes(col) ? s.filter((c) => c !== col) : [...s, col]));
  return (
    <ChangeDialog
      {...props}
      title={`Add index to ${props.table}`}
      change={{ kind: "addIndex", name: name ?? suggested, columns: selected, unique }}
      confirm="Create index"
    >
      <div className="form-grid">
        <label>Columns</label>
        <div className="index-columns">
          {props.columns.map((c) => {
            const pos = selected.indexOf(c.name);
            return (
              <label key={c.name} className="check">
                <input type="checkbox" checked={pos >= 0} onChange={() => toggle(c.name)} />
                <span className="index-pos">{pos >= 0 ? pos + 1 : ""}</span>
                {c.name} <span className="faint mono">{c.dataType}</span>
              </label>
            );
          })}
        </div>
        <span />
        <span className="faint">Columns are indexed in the order you tick them.</span>
        <label>Name</label>
        <input
          value={name ?? suggested}
          spellCheck={false}
          onChange={(e) => setName(e.target.value || null)}
        />
        <span />
        <label className="check">
          <input type="checkbox" checked={unique} onChange={(e) => setUnique(e.target.checked)} />
          Unique
        </label>
      </div>
    </ChangeDialog>
  );
}
