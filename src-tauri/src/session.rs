//! A live connection: a driver (plus SSH tunnel) and the catalog/browsing
//! operations built on top of it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use tokio::sync::RwLock;

use crate::dialect::Dialect;
use crate::drivers::{
    mysql::MysqlDriver, postgres::PostgresDriver, sqlite::SqliteDriver, statement_context, Driver,
};
use crate::model::*;
use crate::ssh::Tunnel;

pub struct Session {
    pub config: ConnectionConfig,
    pub dialect: Dialect,
    driver: RwLock<Arc<dyn Driver>>,
    tunnel: Option<Tunnel>,
    info: RwLock<SessionInfo>,
}

fn default_port(driver: DriverKind) -> u16 {
    match driver {
        DriverKind::Postgres => 5432,
        DriverKind::Mysql => 3306,
        DriverKind::Sqlite => 0,
    }
}

async fn open_driver(
    cfg: &ConnectionConfig,
    database: &str,
    tunnel: Option<&Tunnel>,
) -> Result<Arc<dyn Driver>> {
    Ok(match cfg.driver {
        DriverKind::Postgres => {
            Arc::new(PostgresDriver::connect(cfg, database, tunnel.map(Tunnel::local_addr)).await?)
        }
        DriverKind::Mysql => {
            let mut cfg = cfg.clone();
            cfg.database = database.to_string();
            let addr = tunnel.map(|t| ("127.0.0.1".to_string(), t.local_port));
            Arc::new(MysqlDriver::connect(&cfg, addr).await?)
        }
        DriverKind::Sqlite => Arc::new(SqliteDriver::open(cfg).await?),
    })
}

/// First column of every row of the first result set.
fn first_column(results: Vec<QueryResult>) -> Vec<String> {
    results
        .into_iter()
        .find(|r| r.has_rows)
        .map(|r| r.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
        .unwrap_or_default()
}

fn rows_of(results: Vec<QueryResult>) -> Vec<Vec<Cell>> {
    results.into_iter().find(|r| r.has_rows).map(|r| r.rows).unwrap_or_default()
}

impl Session {
    pub async fn open(id: String, cfg: ConnectionConfig) -> Result<Self> {
        let tunnel = if cfg.ssh.enabled && cfg.driver != DriverKind::Sqlite {
            let target_host = if cfg.host.is_empty() { "127.0.0.1" } else { &cfg.host };
            let target_port = if cfg.port == 0 { default_port(cfg.driver) } else { cfg.port };
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

        let driver = match open_driver(&cfg, &cfg.database, tunnel.as_ref()).await {
            Ok(d) => d,
            Err(e) => {
                if let Some(t) = &tunnel {
                    t.close().await;
                }
                return Err(e);
            }
        };
        let dialect = Dialect(cfg.driver);
        let server_version = first_column(driver.query(dialect.version_sql()).await?)
            .pop()
            .unwrap_or_default();
        let schema = first_column(driver.query(dialect.current_schema_sql()).await?)
            .pop()
            .unwrap_or_default();
        let database = match cfg.driver {
            DriverKind::Postgres => first_column(driver.query("SELECT current_database()").await?)
                .pop()
                .unwrap_or_default(),
            DriverKind::Mysql => schema.clone(),
            DriverKind::Sqlite => cfg.file_path.clone(),
        };

        let info = SessionInfo {
            session_id: id,
            connection_id: cfg.id.clone(),
            name: cfg.name.clone(),
            driver: cfg.driver,
            database,
            schema,
            server_version,
        };
        Ok(Self {
            config: cfg,
            dialect,
            driver: RwLock::new(driver),
            tunnel,
            info: RwLock::new(info),
        })
    }

    pub async fn info(&self) -> SessionInfo {
        self.info.read().await.clone()
    }

    pub async fn driver(&self) -> Arc<dyn Driver> {
        self.driver.read().await.clone()
    }

    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>> {
        self.driver()
            .await
            .query(sql)
            .await
            .map_err(|e| statement_context(e, sql))
    }

    pub async fn close(&self) {
        self.driver().await.close().await;
        if let Some(t) = &self.tunnel {
            t.close().await;
        }
    }

    // ---- catalog -------------------------------------------------------

    pub async fn list_databases(&self) -> Result<Vec<String>> {
        match self.dialect.databases_sql() {
            Some(sql) => Ok(first_column(self.query(sql).await?)),
            None => Ok(Vec::new()),
        }
    }

    /// Postgres can't change database on a live connection, so reconnect
    /// (reusing the SSH tunnel) and swap the driver.
    pub async fn switch_database(&self, database: &str) -> Result<SessionInfo> {
        if self.config.driver != DriverKind::Postgres {
            return Err(anyhow!("switching database is only supported for PostgreSQL"));
        }
        let fresh = open_driver(&self.config, database, self.tunnel.as_ref()).await?;
        let schema = first_column(fresh.query(self.dialect.current_schema_sql()).await?)
            .pop()
            .unwrap_or_else(|| "public".into());
        let old = std::mem::replace(&mut *self.driver.write().await, fresh);
        old.close().await;
        let mut info = self.info.write().await;
        info.database = database.to_string();
        info.schema = schema;
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
        Ok(first_column(self.query(self.dialect.schemas_sql()).await?))
    }

    pub async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>> {
        Ok(rows_of(self.query(&self.dialect.tables_sql(schema)).await?)
            .into_iter()
            .map(|mut row| TableInfo {
                kind: row.pop().flatten().unwrap_or_else(|| "table".into()).to_lowercase(),
                name: row.pop().flatten().unwrap_or_default(),
            })
            .collect())
    }

    pub async fn table_columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        Ok(rows_of(self.query(&self.dialect.columns_sql(schema, table)).await?)
            .into_iter()
            .map(|row| {
                let mut it = row.into_iter();
                let mut next = || it.next().flatten();
                ColumnInfo {
                    name: next().unwrap_or_default(),
                    data_type: next().unwrap_or_default(),
                    nullable: next().as_deref() != Some("NO"),
                    default: next(),
                    is_primary_key: next().as_deref() == Some("YES"),
                }
            })
            .collect())
    }

    pub async fn schema_columns(&self, schema: &str) -> Result<BTreeMap<String, Vec<String>>> {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in rows_of(self.query(&self.dialect.schema_columns_sql(schema)).await?) {
            let mut it = row.into_iter();
            if let (Some(Some(table)), Some(Some(col))) = (it.next(), it.next()) {
                map.entry(table).or_default().push(col);
            }
        }
        Ok(map)
    }

    // ---- browsing & editing --------------------------------------------

    pub async fn fetch_rows(&self, req: &FetchRequest) -> Result<QueryResult> {
        let sql = self.dialect.select_page_sql(req)?;
        let mut result = self
            .query(&sql)
            .await?
            .into_iter()
            .find(|r| r.has_rows)
            .context("the query returned no result set")?;
        result.sql = sql;
        Ok(result)
    }

    pub async fn count_rows(&self, req: &FetchRequest) -> Result<u64> {
        let sql = self.dialect.count_sql(req)?;
        let count = first_column(self.query(&sql).await?).pop().unwrap_or_default();
        count.parse().context("unexpected COUNT(*) result")
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

    pub fn preview_changes(&self, set: &ChangeSet) -> Result<Vec<String>> {
        self.dialect.change_statements(set)
    }

    pub async fn commit_changes(&self, set: &ChangeSet) -> Result<u64> {
        let statements = self.dialect.change_statements(set)?;
        self.driver().await.execute_in_transaction(&statements).await
    }

    pub async fn cancel(&self) -> Result<()> {
        self.driver().await.cancel().await
    }
}
