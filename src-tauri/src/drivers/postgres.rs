use std::net::IpAddr;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use postgres_native_tls::MakeTlsConnector;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_postgres::config::SslMode as PgSslMode;
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage};

use super::{statement_context, Driver, MAX_RESULT_ROWS};
use crate::model::{ColumnMeta, ConnectionConfig, QueryResult, SslMode};

pub struct PostgresDriver {
    client: Client,
    cancel_token: CancelToken,
    tls: MakeTlsConnector,
    connection_task: JoinHandle<()>,
    /// Serializes statements so a commit transaction never interleaves with
    /// queries from other tabs sharing this connection.
    op_lock: Mutex<()>,
}

/// Postgres errors only say "db error" when displayed; surface the server's
/// message, detail and hint instead.
pub fn pg_error(e: tokio_postgres::Error) -> anyhow::Error {
    match e.as_db_error() {
        Some(db) => {
            let mut msg = format!("{}: {}", db.severity(), db.message());
            if let Some(detail) = db.detail() {
                msg.push_str(&format!("\nDETAIL: {detail}"));
            }
            if let Some(hint) = db.hint() {
                msg.push_str(&format!("\nHINT: {hint}"));
            }
            anyhow!(msg)
        }
        None => anyhow!(e),
    }
}

impl PostgresDriver {
    /// `addr` overrides the TCP endpoint (used for SSH tunnels) while TLS still
    /// verifies against the configured host name.
    pub async fn connect(
        cfg: &ConnectionConfig,
        database: &str,
        addr: Option<(IpAddr, u16)>,
    ) -> Result<Self> {
        let port = match addr {
            Some((_, local_port)) => local_port,
            None if cfg.port == 0 => 5432,
            None => cfg.port,
        };
        let mut pg = tokio_postgres::Config::new();
        pg.host(if cfg.host.is_empty() { "localhost" } else { &cfg.host })
            .port(port)
            .user(&cfg.user)
            .dbname(if database.is_empty() { "postgres" } else { database })
            .application_name("HuginnTables")
            .connect_timeout(Duration::from_secs(15))
            .ssl_mode(match cfg.ssl_mode {
                SslMode::Disable => PgSslMode::Disable,
                SslMode::Prefer => PgSslMode::Prefer,
                SslMode::Require | SslMode::VerifyFull => PgSslMode::Require,
            });
        if let Some(pw) = cfg.password.as_deref().filter(|p| !p.is_empty()) {
            pg.password(pw);
        }
        if let Some((ip, _)) = addr {
            pg.hostaddr(ip);
        }

        let mut builder = native_tls::TlsConnector::builder();
        if cfg.ssl_mode != SslMode::VerifyFull {
            builder
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        let tls = MakeTlsConnector::new(builder.build()?);

        let (client, connection) = pg.connect(tls.clone()).await.map_err(pg_error)?;
        let connection_task = tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self {
            cancel_token: client.cancel_token(),
            client,
            tls,
            connection_task,
            op_lock: Mutex::new(()),
        })
    }

    async fn run(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let started = Instant::now();
        let messages = self.client.simple_query(sql).await.map_err(pg_error)?;
        let duration_ms = started.elapsed().as_millis() as u64;

        let mut results = Vec::new();
        let mut current: Option<QueryResult> = None;
        for msg in messages {
            match msg {
                SimpleQueryMessage::RowDescription(cols) => {
                    current = Some(QueryResult {
                        sql: sql.to_string(),
                        columns: cols
                            .iter()
                            .map(|c| ColumnMeta { name: c.name().to_string(), data_type: None })
                            .collect(),
                        has_rows: true,
                        ..Default::default()
                    });
                }
                SimpleQueryMessage::Row(row) => {
                    let result = current.get_or_insert_with(|| QueryResult {
                        sql: sql.to_string(),
                        has_rows: true,
                        columns: row
                            .columns()
                            .iter()
                            .map(|c| ColumnMeta { name: c.name().to_string(), data_type: None })
                            .collect(),
                        ..Default::default()
                    });
                    if result.rows.len() >= MAX_RESULT_ROWS {
                        result.truncated = true;
                        continue;
                    }
                    result
                        .rows
                        .push((0..row.len()).map(|i| row.get(i).map(str::to_string)).collect());
                }
                SimpleQueryMessage::CommandComplete(n) => {
                    let mut result = current.take().unwrap_or_else(|| QueryResult {
                        sql: sql.to_string(),
                        ..Default::default()
                    });
                    result.rows_affected = n;
                    result.duration_ms = duration_ms;
                    results.push(result);
                }
                _ => {}
            }
        }
        if let Some(mut result) = current {
            result.duration_ms = duration_ms;
            results.push(result);
        }
        Ok(results)
    }
}

#[async_trait]
impl Driver for PostgresDriver {
    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let _guard = self.op_lock.lock().await;
        self.run(sql).await
    }

    async fn execute_in_transaction(&self, statements: &[String]) -> Result<u64> {
        let _guard = self.op_lock.lock().await;
        self.client.batch_execute("BEGIN").await.map_err(pg_error)?;
        let mut affected = 0;
        for sql in statements {
            match self.run(sql).await {
                Ok(results) => affected += results.iter().map(|r| r.rows_affected).sum::<u64>(),
                Err(e) => {
                    let _ = self.client.batch_execute("ROLLBACK").await;
                    return Err(statement_context(e, sql));
                }
            }
        }
        self.client.batch_execute("COMMIT").await.map_err(pg_error)?;
        Ok(affected)
    }

    async fn cancel(&self) -> Result<()> {
        self.cancel_token
            .cancel_query(self.tls.clone())
            .await
            .map_err(pg_error)
    }

    async fn close(&self) {
        self.connection_task.abort();
    }
}
