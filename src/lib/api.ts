import { invoke } from "@tauri-apps/api/core";

export type DriverKind = "postgres" | "mysql" | "sqlite" | "redis" | "mongodb" | "bigquery";
export type SslMode = "disable" | "prefer" | "require" | "verifyFull";
export type Cell = string | null;

export interface SshConfig {
  enabled: boolean;
  host: string;
  port: number;
  user: string;
  auth: "password" | "privateKey";
  keyPath: string;
}

export interface ConnectionConfig {
  id: string;
  name: string;
  driver: DriverKind;
  color: string;
  /** Template id from lib/templates.ts, used for labels only. */
  template: string;
  /** BigQuery: API endpoint override (emulator); empty for Google. */
  host: string;
  port: number;
  user: string;
  database: string;
  sslMode: SslMode;
  /** SQLite file, or BigQuery service-account key (empty = gcloud credentials). */
  filePath: string;
  /** MongoDB connection string; replaces host/port/user when set. */
  uri: string;
  ssh: SshConfig;
  /** Only sent when changed; otherwise the backend reads the keychain. */
  password?: string;
  sshPassword?: string;
  sshPassphrase?: string;
}

export interface SessionInfo {
  sessionId: string;
  connectionId: string;
  name: string;
  driver: DriverKind;
  database: string;
  schema: string;
  serverVersion: string;
  readOnly: boolean;
}

export interface ColumnMeta {
  name: string;
  dataType?: string;
}

export interface QueryResult {
  sql: string;
  columns: ColumnMeta[];
  rows: Cell[][];
  hasRows: boolean;
  rowsAffected: number;
  durationMs: number;
  truncated: boolean;
  error?: string;
  notice?: string;
}

export interface TableInfo {
  name: string;
  kind: "table" | "view" | "materialized_view" | "foreign_table" | "keyspace" | "collection";
}

export interface ColumnInfo {
  name: string;
  dataType: string;
  nullable: boolean;
  default: string | null;
  isPrimaryKey: boolean;
}

export type FilterOp =
  | "=" | "<>" | "<" | ">" | "<=" | ">="
  | "contains" | "notContains" | "like" | "in" | "isNull" | "isNotNull";

export interface Filter {
  column: string;
  op: FilterOp;
  value: string;
}

export interface FetchRequest {
  schema: string;
  table: string;
  limit: number;
  offset: number;
  filters: Filter[];
  rawWhere: string;
  sort: { column: string; desc: boolean } | null;
  /** Primary key columns, used as the default order and as a tiebreaker. */
  keyOrder: string[];
}

/** A `FetchRequest` without paging: which rows to export, in what order. */
export type ExportRequest = Omit<FetchRequest, "limit" | "offset">;

export type ExportFormat = "csv" | "json" | "sql";

export interface CellValue {
  column: string;
  value: Cell;
}

export type RowChange =
  | { kind: "insert"; values: CellValue[] }
  | { kind: "update"; key: CellValue[]; values: CellValue[] }
  | { kind: "delete"; key: CellValue[] };

export interface ChangeSet {
  schema: string;
  table: string;
  keyIsPrimary: boolean;
  /** Also delete every row in other tables that references a deleted row. */
  cascade?: boolean;
  changes: RowChange[];
}

/** Rows of another table that point, through a foreign key, at rows being deleted. */
export interface RowReference {
  schema: string;
  table: string;
  columns: string[];
  parentTable: string;
  /** The constraint's own rule: "NO ACTION", "RESTRICT", "CASCADE", "SET NULL"… */
  onDelete: string;
  /** 1 for direct references, 2 for rows referencing those, … */
  depth: number;
  rows: number;
}

/** What deleting some rows would touch in other tables. */
export interface DeleteImpact {
  /** One entry per foreign key that matches rows, each right after the one it points at. */
  references: RowReference[];
  /** Distinct rows a cascading delete removes from other tables. */
  cascadedRows: number;
}

export const api = {
  listConnections: () => invoke<ConnectionConfig[]>("list_connections"),
  saveConnection: (config: ConnectionConfig) => invoke<ConnectionConfig>("save_connection", { config }),
  deleteConnection: (id: string) => invoke<void>("delete_connection", { id }),
  testConnection: (config: ConnectionConfig) => invoke<string>("test_connection", { config }),
  connect: (config: ConnectionConfig) => invoke<SessionInfo>("connect", { config }),
  disconnect: (sessionId: string) => invoke<void>("disconnect", { sessionId }),
  listDatabases: (sessionId: string) => invoke<string[]>("list_databases", { sessionId }),
  switchDatabase: (sessionId: string, database: string) =>
    invoke<SessionInfo>("switch_database", { sessionId, database }),
  useSchema: (sessionId: string, schema: string) => invoke<void>("use_schema", { sessionId, schema }),
  listSchemas: (sessionId: string) => invoke<string[]>("list_schemas", { sessionId }),
  listTables: (sessionId: string, schema: string) => invoke<TableInfo[]>("list_tables", { sessionId, schema }),
  tableColumns: (sessionId: string, schema: string, table: string) =>
    invoke<ColumnInfo[]>("table_columns", { sessionId, schema, table }),
  schemaColumns: (sessionId: string, schema: string) =>
    invoke<Record<string, string[]>>("schema_columns", { sessionId, schema }),
  fetchRows: (sessionId: string, request: FetchRequest) =>
    invoke<QueryResult>("fetch_rows", { sessionId, request }),
  countRows: (sessionId: string, request: FetchRequest) => invoke<number>("count_rows", { sessionId, request }),
  runQuery: (sessionId: string, statements: string[]) =>
    invoke<QueryResult[]>("run_query", { sessionId, statements }),
  cancelQuery: (sessionId: string) => invoke<void>("cancel_query", { sessionId }),
  deleteReferences: (sessionId: string, changes: ChangeSet) =>
    invoke<DeleteImpact>("delete_references", { sessionId, changes }),
  previewChanges: (sessionId: string, changes: ChangeSet) =>
    invoke<string[]>("preview_changes", { sessionId, changes }),
  commitChanges: (sessionId: string, changes: ChangeSet) =>
    invoke<number>("commit_changes", { sessionId, changes }),
  exportTable: (sessionId: string, request: ExportRequest, format: ExportFormat, path: string) =>
    invoke<number>("export_table", { sessionId, request, format, path }),
  writeTextFile: (path: string, contents: string) => invoke<void>("write_text_file", { path, contents }),
};

export const DEFAULT_PORTS: Record<DriverKind, number> = {
  postgres: 5432, mysql: 3306, sqlite: 0, redis: 6379, mongodb: 27017, bigquery: 0,
};

export const DRIVER_LABELS: Record<DriverKind, string> = {
  postgres: "PostgreSQL",
  mysql: "MySQL / MariaDB",
  sqlite: "SQLite",
  redis: "Redis",
  mongodb: "MongoDB",
  bigquery: "BigQuery",
};

/** Engines whose grid and editor speak SQL. */
export const isSql = (d: DriverKind) => d === "postgres" || d === "mysql" || d === "sqlite" || d === "bigquery";

export function emptyConnection(driver: DriverKind = "postgres"): ConnectionConfig {
  return {
    id: "",
    name: "",
    driver,
    color: "#5b8def",
    template: driver,
    host: driver === "sqlite" || driver === "bigquery" ? "" : "127.0.0.1",
    port: DEFAULT_PORTS[driver],
    user: driver === "postgres" ? "postgres" : driver === "mysql" ? "root" : "",
    database: driver === "redis" ? "0" : "",
    sslMode: driver === "redis" || driver === "mongodb" ? "disable" : "prefer",
    filePath: "",
    uri: "",
    ssh: { enabled: false, host: "", port: 22, user: "", auth: "password", keyPath: "~/.ssh/id_ed25519" },
  };
}

/** Parses postgres://, mysql:// and sqlite:// URLs into a connection. */
export function parseConnectionUrl(raw: string): ConnectionConfig {
  const text = raw.trim();
  const scheme = text.slice(0, text.indexOf(":")).toLowerCase();
  const driver: DriverKind | undefined =
    scheme.startsWith("postgres") ? "postgres"
    : scheme.startsWith("mysql") || scheme.startsWith("mariadb") ? "mysql"
    : scheme.startsWith("sqlite") || scheme === "file" ? "sqlite"
    : scheme === "redis" || scheme === "rediss" ? "redis"
    : scheme.startsWith("mongodb") ? "mongodb"
    : undefined;
  if (!driver) throw new Error(`Unsupported URL scheme "${scheme}"`);
  const cfg = emptyConnection(driver);
  if (driver === "mongodb") {
    // Keep the whole string: SRV records and options don't fit host/port fields.
    const url = new URL(text.replace(/^[^:]+:/, "http:"));
    if (url.password) cfg.password = decodeURIComponent(url.password);
    cfg.uri = url.password ? text.replace(`:${url.password}@`, ":<password>@") : text;
    cfg.name = decodeURIComponent(url.hostname.split(",")[0]);
    return cfg;
  }
  if (driver === "sqlite") {
    cfg.filePath = decodeURIComponent(text.replace(/^[a-z0-9+]+:(\/\/)?/i, ""));
    cfg.name = cfg.filePath.split("/").pop() ?? "SQLite";
    return cfg;
  }
  // Swap the scheme for http so the URL parser treats the host uniformly.
  const url = new URL(text.replace(/^[^:]+:/, "http:"));
  cfg.host = decodeURIComponent(url.hostname);
  cfg.port = url.port ? Number(url.port) : DEFAULT_PORTS[driver];
  cfg.user = decodeURIComponent(url.username) || cfg.user;
  if (url.password) cfg.password = decodeURIComponent(url.password);
  cfg.database = decodeURIComponent(url.pathname.replace(/^\//, "")) || (driver === "redis" ? "0" : "");
  if (scheme === "rediss") cfg.sslMode = "require";
  const ssl = url.searchParams.get("sslmode") ?? url.searchParams.get("ssl-mode");
  if (ssl) {
    const map: Record<string, SslMode> = {
      disable: "disable", disabled: "disable", allow: "prefer", prefer: "prefer", preferred: "prefer",
      require: "require", required: "require", "verify-ca": "verifyFull", "verify-full": "verifyFull",
      verify_identity: "verifyFull",
    };
    cfg.sslMode = map[ssl.toLowerCase()] ?? cfg.sslMode;
  }
  cfg.name = cfg.database && driver !== "redis" ? `${cfg.database} @ ${cfg.host}` : cfg.host;
  return cfg;
}

export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
