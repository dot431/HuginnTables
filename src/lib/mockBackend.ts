/**
 * In-memory stand-in for the Rust backend, installed only by `npm run dev` in a
 * plain browser (outside Tauri) so the UI can be developed and tested there.
 */
import type { Cell, ChangeSet, ConnectionConfig, FetchRequest, ForeignKeyInfo, IndexInfo, QueryResult, SchemaChange } from "./api";

type Table = { columns: { name: string; dataType: string; nullable: boolean; isPrimaryKey: boolean }[]; rows: Cell[][] };

const tables: Record<string, Table> = {
  customers: {
    columns: [
      { name: "id", dataType: "integer", nullable: false, isPrimaryKey: true },
      { name: "name", dataType: "text", nullable: false, isPrimaryKey: false },
      { name: "email", dataType: "text", nullable: true, isPrimaryKey: false },
      { name: "country", dataType: "text", nullable: true, isPrimaryKey: false },
      { name: "meta", dataType: "jsonb", nullable: true, isPrimaryKey: false },
    ],
    rows: Array.from({ length: 1234 }, (_, i) => [
      String(i + 1),
      `Customer ${i + 1}`,
      i % 11 === 0 ? null : `user${i + 1}@example.com`,
      ["NO", "SE", "DK", "FI", "IS"][i % 5],
      JSON.stringify({ tier: i % 7 === 0 ? "gold" : "basic" }),
    ]),
  },
  countries: {
    columns: [
      { name: "code", dataType: "text", nullable: false, isPrimaryKey: true },
      { name: "name", dataType: "text", nullable: false, isPrimaryKey: false },
    ],
    rows: [["NO", "Norway"], ["SE", "Sweden"], ["DK", "Denmark"], ["FI", "Finland"], ["IS", "Iceland"]],
  },
  orders: {
    columns: [
      { name: "id", dataType: "integer", nullable: false, isPrimaryKey: true },
      { name: "customer_id", dataType: "integer", nullable: true, isPrimaryKey: false },
      { name: "total", dataType: "numeric", nullable: false, isPrimaryKey: false },
    ],
    rows: Array.from({ length: 3000 }, (_, i) => [
      String(i + 1),
      i % 50 === 0 ? null : String(((i * 7) % 1234) + 1),
      ((i * 37) % 500 + 9.95).toFixed(2),
    ]),
  },
  audit_log: {
    columns: [
      { name: "at", dataType: "timestamptz", nullable: false, isPrimaryKey: false },
      { name: "message", dataType: "text", nullable: true, isPrimaryKey: false },
    ],
    rows: [["2026-10-01 10:00:00+00", "login"], ["2026-10-01 10:05:00+00", "logout"]],
  },
};

const foreignKeys: ForeignKeyInfo[] = [
  {
    name: "orders_customer_id_fkey", schema: "public", table: "orders", columns: ["customer_id"],
    parentSchema: "public", parentTable: "customers", parentColumns: ["id"], onDelete: "CASCADE",
  },
  {
    name: "customers_country_fkey", schema: "public", table: "customers", columns: ["country"],
    parentSchema: "public", parentTable: "countries", parentColumns: ["code"], onDelete: "NO ACTION",
  },
];

const indexes: Record<string, IndexInfo[]> = {
  customers: [
    { name: "customers_pkey", columns: ["id"], unique: true, primary: true, constraint: true, method: "btree", definition: "CREATE UNIQUE INDEX customers_pkey ON public.customers USING btree (id)" },
    { name: "customers_email_key", columns: ["email"], unique: true, primary: false, constraint: true, method: "btree", definition: "CREATE UNIQUE INDEX customers_email_key ON public.customers USING btree (email)" },
    { name: "customers_country_name_idx", columns: ["country", "name"], unique: false, primary: false, constraint: false, method: "btree", definition: "CREATE INDEX customers_country_name_idx ON public.customers USING btree (country, name)" },
  ],
};

const q = (name: string) => `"${name.replace(/"/g, '""')}"`;

/** Postgres-flavoured stand-in for the dialect's schema change SQL. */
function schemaChangeSql(schema: string, table: string, c: SchemaChange): string[] {
  const target = `${q(schema)}.${q(table)}`;
  if (c.kind === "addColumn") {
    if (!c.name.trim()) throw "enter a column name";
    if (!c.dataType.trim()) throw "enter a column type";
    const def = c.default?.trim() ? ` DEFAULT ${c.default.trim()}` : "";
    return [`ALTER TABLE ${target} ADD COLUMN ${q(c.name.trim())} ${c.dataType.trim()}${def}${c.nullable ? "" : " NOT NULL"}`];
  }
  if (c.kind === "addIndex") {
    if (!c.name.trim()) throw "enter an index name";
    if (!c.columns.length) throw "choose at least one column";
    return [`CREATE ${c.unique ? "UNIQUE " : ""}INDEX ${q(c.name.trim())} ON ${target} (${c.columns.map(q).join(", ")})`];
  }
  return [c.constraint ? `ALTER TABLE ${target} DROP CONSTRAINT ${q(c.name)}` : `DROP INDEX ${q(schema)}.${q(c.name)}`];
}

let connections: ConnectionConfig[] = [];
/** Open sessions' connections, so read-only ones can refuse writes. */
const sessions = new Map<string, ConnectionConfig>();
const delay = <T>(v: T) => new Promise<T>((r) => setTimeout(() => r(v), 60));

function matches(row: Cell[], t: Table, req: FetchRequest) {
  return req.filters.every((f) => {
    const v = row[t.columns.findIndex((c) => c.name === f.column)];
    switch (f.op) {
      case "=": return v === f.value;
      case "contains": return (v ?? "").toLowerCase().includes(f.value.toLowerCase());
      case "isNull": return v === null;
      case "isNotNull": return v !== null;
      default: return true;
    }
  });
}

function select(req: FetchRequest): Cell[][] {
  const t = tables[req.table];
  let rows = t.rows.filter((r) => matches(r, t, req));
  if (req.sort) {
    const i = t.columns.findIndex((c) => c.name === req.sort!.column);
    const num = t.columns[i].dataType === "integer";
    rows = [...rows].sort((a, b) => {
      const x = num ? Number(a[i]) - Number(b[i]) : String(a[i]).localeCompare(String(b[i]));
      return req.sort!.desc ? -x : x;
    });
  }
  return rows;
}

function applyChanges(set: ChangeSet): number {
  const t = tables[set.table];
  const idx = (c: string) => t.columns.findIndex((x) => x.name === c);
  const find = (key: { column: string; value: Cell }[]) =>
    t.rows.findIndex((r) => key.every((k) => r[idx(k.column)] === k.value));
  for (const ch of set.changes) {
    if (ch.kind === "update") {
      const r = find(ch.key);
      if (r < 0) throw "row not found";
      for (const v of ch.values) {
        if (v.value === null && !t.columns[idx(v.column)].nullable) throw `ERROR: null value in column "${v.column}" violates not-null constraint`;
        t.rows[r][idx(v.column)] = v.value;
      }
    } else if (ch.kind === "delete") {
      t.rows.splice(find(ch.key), 1);
    } else {
      const row: Cell[] = t.columns.map(() => null);
      for (const v of ch.values) row[idx(v.column)] = v.value;
      if (row[0] === null && t.columns[0].isPrimaryKey) row[0] = String(t.rows.length + 1);
      t.rows.push(row);
    }
  }
  return set.changes.length;
}

const handlers: Record<string, (a: any) => unknown> = {
  list_connections: () => connections,
  save_connection: ({ config }) => {
    const c = { ...config, id: config.id || crypto.randomUUID(), password: undefined };
    connections = [...connections.filter((x) => x.id !== c.id), c];
    return c;
  },
  delete_connection: ({ id }) => void (connections = connections.filter((c) => c.id !== id)),
  test_connection: () => "17.6 (mock)",
  // A Sail project, as read from <folder>/.env.
  read_env_connection: ({ path }: { path: string }): ConnectionConfig => {
    const folder = path.replace(/\/\.env[^/]*$/, "").split("/").filter(Boolean).pop() ?? "app";
    return {
      id: "", name: `${folder[0].toUpperCase()}${folder.slice(1)} (local)`, driver: "mysql", color: "", template: "mysql",
      host: "127.0.0.1", port: 3306, user: "sail", database: folder, sslMode: "prefer", filePath: "", uri: "",
      ssh: { enabled: false, host: "", port: 22, user: "", auth: "password", keyPath: "~/.ssh/id_ed25519" },
      password: "password",
    };
  },
  connect: ({ config }) => {
    const sessionId = crypto.randomUUID();
    sessions.set(sessionId, config);
    return {
      sessionId, connectionId: config.id, name: config.name, driver: config.driver,
      database: "postgres", schema: "public", serverVersion: "17.6 (mock)",
      readOnly: !!config.readOnly, production: !!config.production,
    };
  },
  disconnect: ({ sessionId }) => void sessions.delete(sessionId),
  list_databases: () => ["postgres", "analytics"],
  switch_database: ({ database }) => ({ database, schema: "public" }),
  use_schema: () => undefined,
  list_schemas: () => ["public", "analytics"],
  list_tables: () => [
    { name: "audit_log", kind: "table" },
    { name: "countries", kind: "table" },
    { name: "customers", kind: "table" },
    { name: "orders", kind: "table" },
    { name: "active_customers", kind: "view" },
  ],
  table_columns: ({ table }) => (tables[table] ?? tables.customers).columns.map((c) => ({ ...c, default: null })),
  schema_columns: () => Object.fromEntries(Object.entries(tables).map(([k, t]) => [k, t.columns.map((c) => c.name)])),
  fetch_rows: ({ request }: { request: FetchRequest }): QueryResult => {
    const req = tables[request.table] ? request : { ...request, table: "customers" };
    const t = tables[req.table];
    return {
      sql: `SELECT * FROM "${req.schema}"."${req.table}" LIMIT ${req.limit} OFFSET ${req.offset}`,
      columns: t.columns.map((c) => ({ name: c.name })),
      rows: select(req).slice(req.offset, req.offset + req.limit).map((r) => [...r]),
      hasRows: true, rowsAffected: 0, durationMs: 3, truncated: false,
    };
  },
  count_rows: ({ request }) => select(tables[request.table] ? request : { ...request, table: "customers" }).length,
  run_query: ({ sessionId, statements }: { sessionId: string; statements: string[] }) =>
    statements.map((sql): QueryResult =>
      sessions.get(sessionId)?.readOnly && !/^\s*select/i.test(sql)
        ? { sql, columns: [], rows: [], hasRows: false, rowsAffected: 0, durationMs: 0, truncated: false, error: "This connection is read-only; the statement was not run." }
        : /^\s*select/i.test(sql)
          ? { sql, columns: [{ name: "n" }, { name: "label" }], rows: [["1", "one"], ["2", null]], hasRows: true, rowsAffected: 2, durationMs: 1, truncated: false }
          : /fail/i.test(sql)
            ? { sql, columns: [], rows: [], hasRows: false, rowsAffected: 0, durationMs: 1, truncated: false, error: 'ERROR: relation "fail" does not exist' }
            : { sql, columns: [], rows: [], hasRows: false, rowsAffected: 3, durationMs: 1, truncated: false },
    ),
  cancel_query: () => undefined,
  // Pretend every customer has two orders with three lines each, and an invoice.
  delete_references: ({ changes }: { changes: ChangeSet }) => {
    const n = changes.table === "customers" ? changes.changes.filter((c) => c.kind === "delete").length : 0;
    return n === 0 ? { references: [], cascadedRows: 0 } : {
      references: [
        { schema: changes.schema, table: "orders", columns: ["customer_id"], parentTable: "customers", onDelete: "NO ACTION", depth: 1, rows: 2 * n },
        { schema: changes.schema, table: "order_lines", columns: ["order_id"], parentTable: "orders", onDelete: "CASCADE", depth: 2, rows: 6 * n },
        { schema: changes.schema, table: "invoices", columns: ["customer_id"], parentTable: "customers", onDelete: "SET NULL", depth: 1, rows: n },
      ],
      cascadedRows: 8 * n,
    };
  },
  table_foreign_keys: ({ table }) => ({
    outgoing: foreignKeys.filter((f) => f.table === table),
    incoming: foreignKeys.filter((f) => f.parentTable === table),
  }),
  preview_changes: ({ changes }) => changes.changes.map((c: { kind: string }) => `-- ${c.kind} ${JSON.stringify(c)}`),
  commit_changes: ({ sessionId, changes }) => {
    if (sessions.get(sessionId)?.readOnly) throw "this connection is read-only";
    return applyChanges(changes);
  },
  table_indexes: ({ table }) => indexes[table] ?? [],
  table_ddl: ({ schema, table }) => {
    const t = tables[table] ?? tables.customers;
    const cols = t.columns.map((c) => `  ${q(c.name)} ${c.dataType}${c.nullable ? "" : " NOT NULL"}`);
    const pk = t.columns.filter((c) => c.isPrimaryKey).map((c) => c.name);
    if (pk.length) cols.push(`  CONSTRAINT ${q(`${table}_pkey`)} PRIMARY KEY (${pk.join(", ")})`);
    const extra = (indexes[table] ?? []).filter((i) => !i.constraint).map((i) => `\n\n${i.definition};`);
    return `CREATE TABLE ${q(schema)}.${q(table)} (\n${cols.join(",\n")}\n);${extra.join("")}`;
  },
  schema_change_sql: ({ schema, table, change }) => schemaChangeSql(schema, table, change),
};

export function installMockBackend() {
  (window as any).__TAURI_INTERNALS__ = {
    invoke: async (cmd: string, args: any) => {
      const h = handlers[cmd];
      if (!h) throw `mock backend: unknown command ${cmd}`;
      return delay(h(args));
    },
    transformCallback: () => 0,
  };
  console.info("HuginnTables: using the in-browser mock backend");
}
