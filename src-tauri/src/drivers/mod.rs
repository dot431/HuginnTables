//! Engine drivers. A `Driver` runs text in the engine's own query language
//! (SQL, Redis commands, MongoDB shell calls); a `Browser` provides the catalog,
//! paged table data and staged edits that the grid needs. SQL engines share one
//! `Browser` (`sql::SqlBrowser`) built on `dialect`; the others bring their own.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::dialect::Dialect;
use crate::model::*;

pub mod bigquery;
pub mod mongo;
pub mod mysql;
pub mod postgres;
pub mod redis;
pub mod sql;
pub mod sqlite;

/// Result sets larger than this are cut off before being sent to the UI.
pub const MAX_RESULT_ROWS: usize = 10_000;

#[async_trait]
pub trait Driver: Send + Sync {
    /// Runs one statement/command. Engines that allow it may return several results.
    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>>;

    /// Runs all statements in one transaction, rolling back on the first error.
    /// Returns the total number of affected rows. Only SQL engines need this.
    async fn execute_in_transaction(&self, _statements: &[String]) -> Result<u64> {
        bail!("transactions are not supported by this engine")
    }

    /// Asks the server to stop whatever this connection is running.
    async fn cancel(&self) -> Result<()>;

    /// Switches the default database/schema on the live connection, when the
    /// engine supports doing so without reconnecting.
    async fn use_schema(&self, _schema: &str) -> Result<()> {
        Ok(())
    }

    async fn close(&self);
}

#[async_trait]
pub trait Browser: Send + Sync {
    /// Databases that need a reconnect to switch to (Postgres only).
    async fn list_databases(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn list_schemas(&self) -> Result<Vec<String>>;
    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>>;
    async fn table_columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>>;
    /// Table → column names, for autocomplete.
    async fn schema_columns(&self, schema: &str) -> Result<BTreeMap<String, Vec<String>>>;
    async fn fetch_rows(&self, req: &FetchRequest) -> Result<QueryResult>;
    async fn count_rows(&self, req: &FetchRequest) -> Result<u64>;
    /// Rows in other tables that point, through foreign keys, at the rows
    /// `set` deletes. Engines without foreign keys have none.
    async fn delete_references(&self, _set: &ChangeSet) -> Result<DeleteImpact> {
        Ok(DeleteImpact::default())
    }
    async fn preview_changes(&self, set: &ChangeSet) -> Result<Vec<String>>;
    async fn commit_changes(&self, set: &ChangeSet) -> Result<u64>;
}

/// A connected engine and what the UI needs to know about it.
pub struct Engine {
    pub driver: Arc<dyn Driver>,
    pub browser: Arc<dyn Browser>,
    pub server_version: String,
    pub database: String,
    pub schema: String,
    pub read_only: bool,
}

/// Connects to `cfg`, optionally through a local tunnel endpoint `addr`.
pub async fn connect(cfg: &ConnectionConfig, database: &str, addr: Option<(IpAddr, u16)>) -> Result<Engine> {
    match cfg.driver {
        DriverKind::Postgres | DriverKind::Mysql | DriverKind::Sqlite => {
            let dialect = Dialect::for_driver(cfg.driver).expect("SQL engine");
            let driver: Arc<dyn Driver> = match cfg.driver {
                DriverKind::Postgres => Arc::new(postgres::PostgresDriver::connect(cfg, database, addr).await?),
                DriverKind::Mysql => {
                    let mut cfg = cfg.clone();
                    cfg.database = database.to_string();
                    Arc::new(mysql::MysqlDriver::connect(&cfg, addr.map(|(ip, p)| (ip.to_string(), p))).await?)
                }
                _ => Arc::new(sqlite::SqliteDriver::open(cfg).await?),
            };
            sql::engine(cfg, driver, dialect).await
        }
        DriverKind::Redis => redis::connect(cfg, addr).await,
        DriverKind::Mongodb => mongo::connect(cfg, addr).await,
        DriverKind::Bigquery => bigquery::connect(cfg).await,
    }
}

/// Formats binary data the way Postgres shows bytea.
pub fn hex_cell(bytes: &[u8]) -> String {
    format!("\\x{}", hex::encode(bytes))
}

pub fn statement_context(err: anyhow::Error, sql: &str) -> anyhow::Error {
    let short: String = sql.chars().take(200).collect();
    err.context(format!("while running: {short}"))
}

/// Builds a result set from rows of cells, capping it at `MAX_RESULT_ROWS`.
pub fn table_result(sql: &str, columns: Vec<String>, mut rows: Vec<Vec<Cell>>) -> QueryResult {
    let truncated = rows.len() > MAX_RESULT_ROWS;
    rows.truncate(MAX_RESULT_ROWS);
    QueryResult {
        sql: sql.to_string(),
        columns: columns.into_iter().map(|name| ColumnMeta { name, data_type: None }).collect(),
        rows,
        has_rows: true,
        truncated,
        ..Default::default()
    }
}

/// A "command OK" result without rows.
pub fn message_result(sql: &str, affected: u64) -> QueryResult {
    QueryResult { sql: sql.to_string(), rows_affected: affected, ..Default::default() }
}
