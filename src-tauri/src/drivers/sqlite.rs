use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, InterruptHandle, OpenFlags};

use super::{hex_cell, statement_context, Driver, MAX_RESULT_ROWS};
use crate::model::{ColumnMeta, ConnectionConfig, QueryResult};

pub struct SqliteDriver {
    conn: Arc<Mutex<Option<Connection>>>,
    interrupt: InterruptHandle,
}

fn cell(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(hex_cell(b)),
    }
}

fn run(conn: &Connection, sql: &str) -> Result<QueryResult> {
    let started = Instant::now();
    let mut stmt = conn.prepare(sql)?;
    let columns: Vec<ColumnMeta> = stmt
        .columns()
        .iter()
        .map(|c| ColumnMeta {
            name: c.name().to_string(),
            data_type: c.decl_type().map(str::to_string),
        })
        .collect();
    let mut result = QueryResult { sql: sql.to_string(), ..Default::default() };
    if columns.is_empty() {
        result.rows_affected = stmt.execute([])? as u64;
    } else {
        let width = columns.len();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if result.rows.len() >= MAX_RESULT_ROWS {
                result.truncated = true;
                break;
            }
            result
                .rows
                .push((0..width).map(|i| row.get_ref(i).map(cell)).collect::<Result<_, _>>()?);
        }
        result.has_rows = true;
        result.columns = columns;
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    Ok(result)
}

impl SqliteDriver {
    pub async fn open(cfg: &ConnectionConfig) -> Result<Self> {
        let path = cfg.file_path.clone();
        if path.trim().is_empty() {
            return Err(anyhow!("choose a database file"));
        }
        let conn = tokio::task::spawn_blocking(move || {
            Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        })
        .await??;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            interrupt: conn.get_interrupt_handle(),
            conn: Arc::new(Mutex::new(Some(conn))),
        })
    }

    async fn with_conn<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().map_err(|_| anyhow!("connection poisoned"))?;
            let conn = guard.as_ref().ok_or_else(|| anyhow!("connection closed"))?;
            f(conn)
        })
        .await?
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let sql = sql.to_string();
        self.with_conn(move |conn| Ok(vec![run(conn, &sql)?])).await
    }

    async fn execute_in_transaction(&self, statements: &[String]) -> Result<u64> {
        let statements = statements.to_vec();
        self.with_conn(move |conn| {
            conn.execute_batch("BEGIN")?;
            let mut affected = 0;
            for sql in &statements {
                match conn.execute(sql, []) {
                    Ok(n) => affected += n as u64,
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Err(statement_context(e.into(), sql));
                    }
                }
            }
            conn.execute_batch("COMMIT")?;
            Ok(affected)
        })
        .await
    }

    async fn cancel(&self) -> Result<()> {
        self.interrupt.interrupt();
        Ok(())
    }

    async fn close(&self) {
        let conn = self.conn.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(mut guard) = conn.lock() {
                guard.take();
            }
        })
        .await;
    }
}
