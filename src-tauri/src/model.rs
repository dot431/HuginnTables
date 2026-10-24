use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    Postgres,
    Mysql,
    Sqlite,
    Redis,
    Mongodb,
    Bigquery,
}

impl DriverKind {
    pub fn default_port(self) -> u16 {
        match self {
            DriverKind::Postgres => 5432,
            DriverKind::Mysql => 3306,
            DriverKind::Redis => 6379,
            DriverKind::Mongodb => 27017,
            DriverKind::Sqlite | DriverKind::Bigquery => 0,
        }
    }

    /// Engines reached over TCP that can be tunnelled through SSH.
    pub fn uses_tcp(self) -> bool {
        !matches!(self, DriverKind::Sqlite | DriverKind::Bigquery)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    /// Encrypt, but don't verify the certificate (libpq `require` semantics).
    Require,
    VerifyFull,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SshAuth {
    #[default]
    Password,
    PrivateKey,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SshConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub key_path: String,
}

/// A saved connection. Secrets travel from the UI in the `password`,
/// `ssh_password` and `ssh_passphrase` fields but are never written to disk:
/// they live in the system keychain.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub driver: DriverKind,
    #[serde(default)]
    pub color: String,
    /// Which preset created this connection (e.g. "aws-rds-postgres"); UI only.
    #[serde(default)]
    pub template: String,
    /// For BigQuery: the API endpoint override (emulators); empty means Google.
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    /// For BigQuery: the project ID.
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub ssl_mode: SslMode,
    /// SQLite database file, or BigQuery service-account key (empty = gcloud ADC).
    #[serde(default)]
    pub file_path: String,
    /// MongoDB connection string; when set it replaces host/port/user.
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub ssh: SshConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_passphrase: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub connection_id: String,
    pub name: String,
    pub driver: DriverKind,
    pub database: String,
    pub schema: String,
    pub server_version: String,
    /// The engine can't edit rows from the grid (BigQuery).
    pub read_only: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
}

pub type Cell = Option<String>;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub sql: String,
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    /// True when the statement produced a result set (even an empty one).
    pub has_rows: bool,
    pub rows_affected: u64,
    pub duration_ms: u64,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Extra information shown with the result, e.g. bytes billed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    /// "table" | "view" | "materialized_view" | "foreign_table"
    pub kind: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_primary_key: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    pub column: String,
    pub op: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sort {
    pub column: String,
    pub desc: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FetchRequest {
    pub schema: String,
    pub table: String,
    pub limit: u64,
    pub offset: u64,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub raw_where: String,
    #[serde(default)]
    pub sort: Option<Sort>,
    /// Primary key columns. Rows are ordered by them (after `sort`) so paging
    /// is stable and edited rows don't move after a commit.
    #[serde(default)]
    pub key_order: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CellValue {
    pub column: String,
    pub value: Cell,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RowChange {
    Insert { values: Vec<CellValue> },
    Update { key: Vec<CellValue>, values: Vec<CellValue> },
    Delete { key: Vec<CellValue> },
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
    pub schema: String,
    pub table: String,
    /// True when `key` columns form the primary key. Otherwise every original
    /// value is used to find the row and only one matching row is touched.
    pub key_is_primary: bool,
    /// Also delete every row in other tables that references a deleted row,
    /// following foreign keys all the way down.
    #[serde(default)]
    pub cascade: bool,
    pub changes: Vec<RowChange>,
}

/// Rows of another table that point, through a foreign key, at rows staged for
/// deletion (directly, or through rows that would be cascaded themselves).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RowReference {
    pub schema: String,
    pub table: String,
    /// The referencing columns of `table`.
    pub columns: Vec<String>,
    /// The table those columns point at.
    pub parent_table: String,
    /// The constraint's own rule: "NO ACTION", "RESTRICT", "CASCADE", "SET NULL"…
    pub on_delete: String,
    /// 1 for rows pointing at the deleted rows, 2 for rows pointing at those, …
    pub depth: u32,
    pub rows: u64,
}
