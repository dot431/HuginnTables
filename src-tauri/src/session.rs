//! A live connection: an engine's driver and browser, plus an optional SSH tunnel.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use tokio::sync::RwLock;

use crate::drivers::{self, Browser, Driver, Engine};
use crate::model::*;
use crate::ssh::Tunnel;

pub struct Session {
    pub config: ConnectionConfig,
    driver: RwLock<Arc<dyn Driver>>,
    browser: RwLock<Arc<dyn Browser>>,
    tunnel: Option<Tunnel>,
    info: RwLock<SessionInfo>,
}

impl Session {
    pub async fn open(id: String, cfg: ConnectionConfig) -> Result<Self> {
        let tunnel = if cfg.ssh.enabled && cfg.driver.uses_tcp() {
            let target_host = if cfg.host.is_empty() { "127.0.0.1" } else { &cfg.host };
            let target_port = if cfg.port == 0 { cfg.driver.default_port() } else { cfg.port };
            Some(
                Tunnel::open(
                    &cfg.ssh,
                    cfg.ssh_password.as_deref(),
                    cfg.ssh_passphrase.as_deref(),
                    target_host,
                    target_port,
                )
                .await?,
            )
        } else {
            None
        };

        let engine = match drivers::connect(&cfg, &cfg.database, tunnel.as_ref().map(Tunnel::local_addr)).await {
            Ok(e) => e,
            Err(e) => {
                if let Some(t) = &tunnel {
                    t.close().await;
                }
                return Err(e);
            }
        };
        let info = SessionInfo {
            session_id: id,
            connection_id: cfg.id.clone(),
            name: cfg.name.clone(),
            driver: cfg.driver,
            database: engine.database,
            schema: engine.schema,
            server_version: engine.server_version,
            read_only: engine.read_only,
        };
        Ok(Self {
            config: cfg,
            driver: RwLock::new(engine.driver),
            browser: RwLock::new(engine.browser),
            tunnel,
            info: RwLock::new(info),
        })
    }

    pub async fn info(&self) -> SessionInfo {
        self.info.read().await.clone()
    }

    async fn driver(&self) -> Arc<dyn Driver> {
        self.driver.read().await.clone()
    }

    async fn browser(&self) -> Arc<dyn Browser> {
        self.browser.read().await.clone()
    }

    pub async fn close(&self) {
        self.driver().await.close().await;
        if let Some(t) = &self.tunnel {
            t.close().await;
        }
    }

    // ---- catalog -------------------------------------------------------

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        self.browser().await.list_databases().await
    }

    /// Postgres can't change database on a live connection, so reconnect
    /// (reusing the SSH tunnel) and swap the engine.
    pub async fn switch_database(&self, database: &str) -> Result<SessionInfo> {
        if self.config.driver != DriverKind::Postgres {
            return Err(anyhow!("switching database is only supported for PostgreSQL"));
        }
        let Engine { driver, browser, schema, .. } =
            drivers::connect(&self.config, database, self.tunnel.as_ref().map(Tunnel::local_addr)).await?;
        let old = std::mem::replace(&mut *self.driver.write().await, driver);
        *self.browser.write().await = browser;
        old.close().await;
        let mut info = self.info.write().await;
        info.database = database.to_string();
        info.schema = if schema.is_empty() { "public".into() } else { schema };
        Ok(info.clone())
    }

    pub async fn use_schema(&self, schema: &str) -> Result<()> {
        self.driver().await.use_schema(schema).await?;
        let mut info = self.info.write().await;
        info.schema = schema.to_string();
        if self.config.driver == DriverKind::Mysql {
            info.database = schema.to_string();
        }
        Ok(())
    }

    pub async fn list_schemas(&self) -> Result<Vec<String>> {
        self.browser().await.list_schemas().await
    }

    pub async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>> {
        self.browser().await.list_tables(schema).await
    }

    pub async fn table_columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        self.browser().await.table_columns(schema, table).await
    }

    pub async fn schema_columns(&self, schema: &str) -> Result<BTreeMap<String, Vec<String>>> {
        self.browser().await.schema_columns(schema).await
    }

    // ---- browsing & editing --------------------------------------------

    pub async fn fetch_rows(&self, req: &FetchRequest) -> Result<QueryResult> {
        self.browser().await.fetch_rows(req).await
    }

    pub async fn count_rows(&self, req: &FetchRequest) -> Result<u64> {
        self.browser().await.count_rows(req).await
    }

    /// Runs statements one by one, stopping at the first failure. The failed
    /// statement is returned as a result carrying the error.
    pub async fn run_statements(&self, statements: &[String]) -> Vec<QueryResult> {
        let driver = self.driver().await;
        let mut out = Vec::new();
        for sql in statements {
            let started = Instant::now();
            match driver.query(sql).await {
                Ok(results) => out.extend(results),
                Err(e) => {
                    out.push(QueryResult {
                        sql: sql.clone(),
                        error: Some(format!("{e:#}")),
                        duration_ms: started.elapsed().as_millis() as u64,
                        ..Default::default()
                    });
                    break;
                }
            }
        }
        out
    }

    pub async fn delete_references(&self, set: &ChangeSet) -> Result<DeleteImpact> {
        self.browser().await.delete_references(set).await
    }

    pub async fn preview_changes(&self, set: &ChangeSet) -> Result<Vec<String>> {
        self.browser().await.preview_changes(set).await
    }

    pub async fn commit_changes(&self, set: &ChangeSet) -> Result<u64> {
        if self.info.read().await.read_only {
            return Err(anyhow!("this connection is read-only"));
        }
        self.browser().await.commit_changes(set).await
    }

    pub async fn cancel(&self) -> Result<()> {
        self.driver().await.cancel().await
    }
}
