/**
 * In-memory stand-in for the Rust backend, installed only by `npm run dev` in a
 * plain browser (outside Tauri) so the UI can be developed and tested there.
 */
import type { Cell, ChangeSet, ConnectionConfig, FetchRequest, QueryResult } from "./api";

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
  audit_log: {
    columns: [
      { name: "at", dataType: "timestamptz", nullable: false, isPrimaryKey: false },
      { name: "message", dataType: "text", nullable: true, isPrimaryKey: false },
    ],
    rows: [["2026-10-01 10:00:00+00", "login"], ["2026-10-01 10:05:00+00", "logout"]],
  },
};

let connections: ConnectionConfig[] = [];
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
  connect: ({ config }) => ({
    sessionId: crypto.randomUUID(), connectionId: config.id, name: config.name, driver: config.driver,
    database: "postgres", schema: "public", serverVersion: "17.6 (mock)", readOnly: false,
  }),
  disconnect: () => undefined,
  list_databases: () => ["postgres", "analytics"],
  switch_database: ({ database }) => ({ database, schema: "public" }),
  use_schema: () => undefined,
  list_schemas: () => ["public", "analytics"],
  list_tables: () => [
    { name: "audit_log", kind: "table" },
    { name: "customers", kind: "table" },
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
  run_query: ({ statements }: { statements: string[] }) =>
    statements.map((sql): QueryResult =>
      /^\s*select/i.test(sql)
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
  preview_changes: ({ changes }) => changes.changes.map((c: { kind: string }) => `-- ${c.kind} ${JSON.stringify(c)}`),
  commit_changes: ({ changes }) => applyChanges(changes),
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
