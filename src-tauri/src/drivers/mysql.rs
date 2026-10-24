use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, Row, SslOpts, Value};
use tokio::sync::Mutex;

use super::{hex_cell, statement_context, Driver, MAX_RESULT_ROWS};
use crate::model::{ColumnMeta, ConnectionConfig, QueryResult, SslMode};

pub struct MysqlDriver {
    conn: Mutex<Option<Conn>>,
    opts: Opts,
    connection_id: u32,
}

fn cell(value: Value) -> Option<String> {
    match value {
        Value::NULL => None,
        Value::Bytes(bytes) => Some(match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => hex_cell(e.as_bytes()),
        }),
        Value::Int(v) => Some(v.to_string()),
        Value::UInt(v) => Some(v.to_string()),
        Value::Float(v) => Some(v.to_string()),
        Value::Double(v) => Some(v.to_string()),
        Value::Date(y, mo, d, h, mi, s, us) => Some(if us > 0 {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
        } else {
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        }),
        Value::Time(neg, days, h, mi, s, us) => {
            let hours = days * 24 + u32::from(h);
            let sign = if neg { "-" } else { "" };
            Some(if us > 0 {
                format!("{sign}{hours:02}:{mi:02}:{s:02}.{us:06}")
            } else {
                format!("{sign}{hours:02}:{mi:02}:{s:02}")
            })
        }
    }
}

impl MysqlDriver {
    pub async fn connect(cfg: &ConnectionConfig, addr: Option<(String, u16)>) -> Result<Self> {
        let (host, port) = addr.unwrap_or_else(|| {
            (
                if cfg.host.is_empty() { "localhost".into() } else { cfg.host.clone() },
                if cfg.port == 0 { 3306 } else { cfg.port },
            )
        });
        let base = OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(port)
            .user(Some(cfg.user.clone()))
            .pass(cfg.password.clone().filter(|p| !p.is_empty()))
            .db_name(Some(cfg.database.clone()).filter(|d| !d.is_empty()))
            .prefer_socket(false);

        let tls = |verify: bool| {
            let mut ssl = SslOpts::default();
            if !verify {
                ssl = ssl
                    .with_danger_accept_invalid_certs(true)
                    .with_danger_skip_domain_validation(true);
            }
            if !cfg.host.is_empty() {
                ssl = ssl.with_danger_tls_hostname_override(Some(cfg.host.clone()));
            }
            ssl
        };

        let attempt = |opts: OptsBuilder| async move {
            let opts = Opts::from(opts);
            let conn = Conn::new(opts.clone()).await?;
            anyhow::Ok((conn, opts))
        };

        let (mut conn, opts) = match cfg.ssl_mode {
            SslMode::Disable => attempt(base).await?,
            SslMode::Require => attempt(base.ssl_opts(tls(false))).await?,
            SslMode::VerifyFull => attempt(base.ssl_opts(tls(true))).await?,
            SslMode::Prefer => match attempt(base.clone().ssl_opts(tls(false))).await {
                Ok(ok) => ok,
                Err(_) => attempt(base).await?,
            },
        };
        if cfg.read_only {
            conn.query_drop("SET SESSION TRANSACTION READ ONLY").await?;
        }
        Ok(Self {
            connection_id: conn.id(),
            conn: Mutex::new(Some(conn)),
            opts,
        })
    }

    async fn run(conn: &mut Conn, sql: &str) -> Result<Vec<QueryResult>> {
        let started = Instant::now();
        let mut results = Vec::new();
        let mut stream = conn.query_iter(sql).await?;
        loop {
            let columns: Vec<ColumnMeta> = stream
                .columns_ref()
                .iter()
                .map(|c| ColumnMeta { name: c.name_str().into_owned(), data_type: None })
                .collect();
            let has_rows = !columns.is_empty();
            let mut rows = Vec::new();
            let mut truncated = false;
            while let Some(row) = stream.next().await? {
                if rows.len() >= MAX_RESULT_ROWS {
                    truncated = true;
                    continue;
                }
                let row: Row = row;
                rows.push(row.unwrap().into_iter().map(cell).collect());
            }
            results.push(QueryResult {
                sql: sql.to_string(),
                columns,
                has_rows,
                rows_affected: stream.affected_rows(),
                truncated,
                rows,
                ..Default::default()
            });
            if stream.is_empty() {
                break;
            }
        }
        let duration_ms = started.elapsed().as_millis() as u64;
        for r in &mut results {
            r.duration_ms = duration_ms;
        }
        Ok(results)
    }
}

#[async_trait]
impl Driver for MysqlDriver {
    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let mut guard = self.conn.lock().await;
        let conn = guard.as_mut().ok_or_else(|| anyhow::anyhow!("connection closed"))?;
        Self::run(conn, sql).await
    }

    async fn execute_in_transaction(&self, statements: &[String]) -> Result<u64> {
        let mut guard = self.conn.lock().await;
        let conn = guard.as_mut().ok_or_else(|| anyhow::anyhow!("connection closed"))?;
        conn.query_drop("START TRANSACTION").await?;
        let mut affected = 0;
        for sql in statements {
            match conn.query_iter(sql.as_str()).await {
                Ok(result) => {
                    affected += result.affected_rows();
                    result.drop_result().await?;
                }
                Err(e) => {
                    let _ = conn.query_drop("ROLLBACK").await;
                    return Err(statement_context(e.into(), sql));
                }
            }
        }
        conn.query_drop("COMMIT").await?;
        Ok(affected)
    }

    async fn cancel(&self) -> Result<()> {
        let mut killer = Conn::new(self.opts.clone()).await?;
        killer
            .query_drop(format!("KILL QUERY {}", self.connection_id))
            .await?;
        killer.disconnect().await?;
        Ok(())
    }

    async fn use_schema(&self, schema: &str) -> Result<()> {
        let mut guard = self.conn.lock().await;
        let conn = guard.as_mut().ok_or_else(|| anyhow::anyhow!("connection closed"))?;
        conn.query_drop(format!("USE `{}`", schema.replace('`', "``"))).await?;
        Ok(())
    }

    async fn close(&self) {
        if let Some(conn) = self.conn.lock().await.take() {
            let _ = conn.disconnect().await;
        }
    }
}
