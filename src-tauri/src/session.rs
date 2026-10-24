//! A live connection: an engine's driver and browser, plus an optional SSH tunnel.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use mongodb::bson::Bson;
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
            read_only: engine.read_only || cfg.read_only,
            production: cfg.production,
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

    pub async fn table_foreign_keys(&self, schema: &str, table: &str) -> Result<TableForeignKeys> {
        self.browser().await.table_foreign_keys(schema, table).await
    }

    pub async fn table_indexes(&self, schema: &str, table: &str) -> Result<Vec<IndexInfo>> {
        self.browser().await.table_indexes(schema, table).await
    }

    pub async fn table_ddl(&self, schema: &str, table: &str) -> Result<String> {
        self.browser().await.table_ddl(schema, table).await
    }

    pub async fn schema_change_sql(&self, schema: &str, table: &str, change: &SchemaChange) -> Result<Vec<String>> {
        self.browser().await.schema_change_sql(schema, table, change).await
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
            if self.config.read_only && refused_when_read_only(self.config.driver, sql) {
                out.push(QueryResult {
                    sql: sql.clone(),
                    error: Some("This connection is read-only; the statement was not run.".into()),
                    ..Default::default()
                });
                break;
            }
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

/// Statements that change data or schema start with one of these.
const WRITE_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "REPLACE", "CREATE", "ALTER", "DROP", "TRUNCATE",
    "RENAME", "GRANT", "REVOKE", "COMMENT", "CALL", "DO", "COPY", "LOAD", "IMPORT", "REFRESH", "REINDEX",
    "VACUUM", "CLUSTER", "EXPORT",
];

/// Redis commands that change keys or server state.
const REDIS_WRITES: &[&str] = &[
    "SET", "SETNX", "SETEX", "PSETEX", "MSET", "MSETNX", "GETSET", "GETDEL", "GETEX", "APPEND", "SETRANGE",
    "INCR", "INCRBY", "INCRBYFLOAT", "DECR", "DECRBY", "DEL", "UNLINK", "EXPIRE", "EXPIREAT", "PEXPIRE",
    "PEXPIREAT", "PERSIST", "RENAME", "RENAMENX", "MOVE", "COPY", "RESTORE", "HSET", "HSETNX", "HMSET", "HDEL",
    "HINCRBY", "HINCRBYFLOAT", "LPUSH", "LPUSHX", "RPUSH", "RPUSHX", "LPOP", "RPOP", "LSET", "LREM", "LTRIM",
    "LINSERT", "LMOVE", "RPOPLPUSH", "BLPOP", "BRPOP", "BLMOVE", "SADD", "SREM", "SPOP", "SMOVE", "SINTERSTORE",
    "SUNIONSTORE", "SDIFFSTORE", "ZADD", "ZREM", "ZINCRBY", "ZPOPMIN", "ZPOPMAX", "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE", "ZREMRANGEBYLEX", "ZUNIONSTORE", "ZINTERSTORE", "ZDIFFSTORE", "ZRANGESTORE", "XADD",
    "XDEL", "XTRIM", "XGROUP", "XACK", "XCLAIM", "XAUTOCLAIM", "PFADD", "PFMERGE", "SETBIT", "BITOP", "BITFIELD",
    "GEOADD", "JSON.SET", "JSON.DEL", "FLUSHDB", "FLUSHALL", "EVAL", "EVALSHA", "FCALL", "CONFIG", "SWAPDB",
];

/// MongoDB shell methods and database commands that change data or schema.
const MONGO_WRITES: &[&str] = &[
    "insertOne", "insertMany", "updateOne", "updateMany", "replaceOne", "deleteOne", "deleteMany", "drop",
    "createIndex", "insert", "update", "delete", "findAndModify", "create", "createIndexes", "dropIndexes",
    "dropDatabase", "renameCollection", "collMod", "eval",
];

/// On a read-only connection, refuses writes from the query editor before they
/// reach the server. Postgres, MySQL and SQLite also enforce read-only mode
/// themselves; this catches the statements that would turn it off again, and
/// is the only guard for BigQuery, Redis and MongoDB.
fn refused_when_read_only(driver: DriverKind, sql: &str) -> bool {
    match driver {
        DriverKind::Redis => {
            let args: Vec<String> = drivers::redis::tokenize(sql)
                .unwrap_or_default()
                .iter()
                .take(2)
                .map(|a| a.to_uppercase())
                .collect();
            return match args.as_slice() {
                [config, get] if config == "CONFIG" && get == "GET" => false,
                [name, ..] => REDIS_WRITES.contains(&name.as_str()),
                [] => false,
            };
        }
        DriverKind::Mongodb => {
            // Shell calls by method name; database commands by their first key.
            let text = sql.trim().trim_end_matches(';').trim();
            let command = |b: Option<&Bson>| match b {
                Some(Bson::Document(d)) => d.keys().next().cloned(),
                _ => None,
            };
            let name = if text.starts_with('{') {
                command(drivers::mongo::parse_json5(text).ok().as_ref())
            } else {
                drivers::mongo::parse_call(text).ok().and_then(|call| match call.method.as_str() {
                    "runCommand" | "adminCommand" => command(call.args.first()),
                    _ => Some(call.method),
                })
            };
            return name.is_some_and(|n| MONGO_WRITES.contains(&n.as_str()));
        }
        _ => {}
    }
    let code = strip_sql(sql, matches!(driver, DriverKind::Mysql | DriverKind::Bigquery)).to_uppercase();
    let words: Vec<&str> = code
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    let lifts_read_only = words.windows(2).any(|w| w == ["READ", "WRITE"])
        || words.iter().any(|w| w.ends_with("TRANSACTION_READ_ONLY") || *w == "TX_READ_ONLY");
    lifts_read_only || words.first().is_some_and(|w| WRITE_KEYWORDS.contains(w))
}

/// The statement without comments and with every quoted string or
/// identifier blanked, so only keywords and names are left. `mysql` adds #
/// comments and backslash escapes (MySQL and BigQuery).
fn strip_sql(sql: &str, mysql: bool) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '-' | '#' if (c == '#' && mysql) || (c == '-' && chars.peek() == Some(&'-')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                out.push(' ');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                out.push(' ');
            }
            '\'' | '"' | '`' => {
                while let Some(n) = chars.next() {
                    if mysql && n == '\\' && c != '`' {
                        chars.next();
                    } else if n == c {
                        if chars.peek() == Some(&c) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                out.push_str(" _ ");
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_writes_on_read_only_connections() {
        let pg = DriverKind::Postgres;
        assert!(!refused_when_read_only(pg, "SELECT * FROM t WHERE note = 'drop table'"));
        assert!(!refused_when_read_only(pg, "-- delete later\nSELECT 1"));
        assert!(!refused_when_read_only(pg, "SET search_path = app"));
        assert!(refused_when_read_only(pg, "/* hi */ delete from t"));
        assert!(refused_when_read_only(pg, "SET default_transaction_read_only = off"));
        assert!(refused_when_read_only(pg, "begin read  write"));
        assert!(refused_when_read_only(DriverKind::Mysql, "SET SESSION TRANSACTION READ WRITE"));
        assert!(refused_when_read_only(DriverKind::Mysql, "set @@session.transaction_read_only = 0"));
        assert!(refused_when_read_only(DriverKind::Bigquery, "INSERT INTO `p.d.t` VALUES (1)"));
        assert!(refused_when_read_only(DriverKind::Redis, "del key"));
        assert!(!refused_when_read_only(DriverKind::Redis, "GET key"));
        assert!(!refused_when_read_only(DriverKind::Redis, "config get maxmemory"));
        let mongo = DriverKind::Mongodb;
        assert!(refused_when_read_only(mongo, "db.users.deleteMany({})"));
        assert!(refused_when_read_only(mongo, "db.runCommand({ drop: 'users' })"));
        assert!(refused_when_read_only(mongo, "{ insert: 'users', documents: [{}] }"));
        assert!(!refused_when_read_only(mongo, "db.users.find({ deleted: true })"));
        assert!(!refused_when_read_only(mongo, "show collections"));
    }
}
