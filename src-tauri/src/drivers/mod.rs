//! Engine drivers. Each driver only knows how to run SQL text and report
//! results; all engine-specific SQL lives in `dialect`.

use anyhow::Result;
use async_trait::async_trait;

use crate::model::QueryResult;

pub mod mysql;
pub mod postgres;
pub mod sqlite;

/// Result sets larger than this are cut off before being sent to the UI.
pub const MAX_RESULT_ROWS: usize = 10_000;

#[async_trait]
pub trait Driver: Send + Sync {
    /// Runs one SQL statement. Engines that allow it may return several results.
    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>>;

    /// Runs all statements in one transaction, rolling back on the first error.
    /// Returns the total number of affected rows.
    async fn execute_in_transaction(&self, statements: &[String]) -> Result<u64>;

    /// Asks the server to stop whatever this connection is running.
    async fn cancel(&self) -> Result<()>;

    /// Switches the default database/schema on the live connection, when the
    /// engine supports doing so without reconnecting.
    async fn use_schema(&self, _schema: &str) -> Result<()> {
        Ok(())
    }

    async fn close(&self);
}

/// Formats binary data the way Postgres shows bytea.
pub fn hex_cell(bytes: &[u8]) -> String {
    format!("\\x{}", hex::encode(bytes))
}

pub fn statement_context(err: anyhow::Error, sql: &str) -> anyhow::Error {
    let short: String = sql.chars().take(200).collect();
    err.context(format!("while running: {short}"))
}
