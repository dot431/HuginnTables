//! Redis: databases are `db0`…`dbN`, "tables" are key prefixes (`user:*`), and
//! each row is one key with its type, TTL and value. Hashes, lists, sets and
//! sorted sets are shown and edited as JSON. Edits run in one MULTI/EXEC.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use redis::{Client, ConnectionAddr, IntoConnectionInfo, RedisConnectionInfo, Value};
use serde_json::{json, Value as Json};
use tokio::sync::Mutex;

use super::{message_result, table_result, Browser, Driver, Engine};
use crate::model::*;

/// Keys scanned at most when listing prefixes or paging a pattern.
const SCAN_LIMIT: usize = 50_000;
/// Elements fetched per list / sorted set when showing a value.
const ELEMENT_LIMIT: isize = 1000;

pub struct RedisEngine {
    addr: ConnectionAddr,
    settings: RedisConnectionInfo,
    conns: Mutex<HashMap<i64, MultiplexedConnection>>,
    /// Database used by the query console (changed with SELECT or the picker).
    current_db: Mutex<i64>,
    databases: i64,
}

pub async fn connect(cfg: &ConnectionConfig, tunnel: Option<(IpAddr, u16)>) -> Result<Engine> {
    let (host, port) = match tunnel {
        Some((ip, p)) => (ip.to_string(), p),
        None => (
            if cfg.host.is_empty() { "127.0.0.1".into() } else { cfg.host.clone() },
            if cfg.port == 0 { 6379 } else { cfg.port },
        ),
    };
    let addr = match cfg.ssl_mode {
        SslMode::Disable | SslMode::Prefer => ConnectionAddr::Tcp(host, port),
        SslMode::Require => ConnectionAddr::TcpTls { host, port, insecure: true, tls_params: None },
        SslMode::VerifyFull => ConnectionAddr::TcpTls { host, port, insecure: false, tls_params: None },
    };
    let mut settings = RedisConnectionInfo::default();
    if !cfg.user.is_empty() {
        settings = settings.set_username(&cfg.user);
    }
    if let Some(pw) = cfg.password.as_deref().filter(|p| !p.is_empty()) {
        settings = settings.set_password(pw);
    }
    let db: i64 = cfg.database.trim_start_matches("db").parse().unwrap_or(0);

    let mut engine = RedisEngine {
        addr,
        settings,
        conns: Mutex::new(HashMap::new()),
        current_db: Mutex::new(db),
        databases: 16,
    };
    let mut conn = engine.conn(db).await?;
    let info: String = redis::cmd("INFO").arg("server").query_async(&mut conn).await?;
    let field = |name: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}:")))
            .map(|v| v.trim().to_string())
    };
    let server_version = match (field("valkey_version"), field("redis_version")) {
        (Some(v), _) => format!("Valkey {v}"),
        (None, Some(v)) => format!("Redis {v}"),
        _ => "Redis".into(),
    };
    // Managed services often block CONFIG; fall back to the usual 16.
    let configured: redis::RedisResult<Vec<String>> =
        redis::cmd("CONFIG").arg("GET").arg("databases").query_async(&mut conn).await;
    if let Some(n) = configured.ok().and_then(|v| v.get(1).and_then(|n| n.parse().ok())) {
        engine.databases = n;
    }

    let engine = Arc::new(engine);
    Ok(Engine {
        driver: engine.clone(),
        browser: engine,
        server_version,
        database: format!("db{db}"),
        schema: format!("db{db}"),
        read_only: false,
    })
}

fn parse_db(schema: &str) -> Result<i64> {
    schema
        .trim_start_matches("db")
        .parse()
        .with_context(|| format!("unknown Redis database {schema:?}"))
}

/// Splits a console line into arguments, honouring single and double quotes.
pub fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut chars = line.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut arg = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            match c {
                '"' | '\'' => loop {
                    match chars.next() {
                        None => bail!("unterminated quote in: {line}"),
                        Some(q) if q == c => break,
                        Some('\\') if c == '"' => match chars.next() {
                            Some('n') => arg.push('\n'),
                            Some('t') => arg.push('\t'),
                            Some(other) => arg.push(other),
                            None => bail!("unterminated quote in: {line}"),
                        },
                        Some(other) => arg.push(other),
                    }
                },
                _ => arg.push(c),
            }
        }
        args.push(arg);
    }
    Ok(args)
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "_:-.*".contains(c)) {
        arg.to_string()
    } else {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
    }
}

fn bytes_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => super::hex_cell(b),
    }
}

/// A reply as a single display string (nested values become JSON).
fn to_text(v: &Value) -> Option<String> {
    match v {
        Value::Nil => None,
        Value::BulkString(b) => Some(bytes_text(b)),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Okay => Some("OK".into()),
        Value::Int(i) => Some(i.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        other => Some(to_json(other).to_string()),
    }
}

fn to_json(v: &Value) -> Json {
    match v {
        Value::Nil => Json::Null,
        Value::Int(i) => json!(i),
        Value::Double(d) => json!(d),
        Value::Boolean(b) => json!(b),
        Value::Array(items) | Value::Set(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Map(pairs) => Json::Object(
            pairs
                .iter()
                .map(|(k, v)| (to_text(k).unwrap_or_default(), to_json(v)))
                .collect(),
        ),
        other => to_text(other).map(Json::String).unwrap_or(Json::Null),
    }
}

/// Flat [k1, v1, k2, v2…] replies (HGETALL on RESP2, ZRANGE WITHSCORES) as pairs.
fn pairs(v: &Value) -> Vec<(String, Value)> {
    match v {
        Value::Map(p) => p.iter().map(|(k, v)| (to_text(k).unwrap_or_default(), v.clone())).collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (to_text(&c[0]).unwrap_or_default(), c[1].clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Converts a pattern filter to a glob, escaping glob characters in literals.
fn glob_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| if "*?[]\\".contains(c) { vec!['\\', c] } else { vec![c] })
        .collect()
}

struct KeyQuery {
    pattern: String,
    key_type: Option<String>,
}

fn key_query(req: &FetchRequest) -> Result<KeyQuery> {
    let mut pattern = if req.table.is_empty() { "*".to_string() } else { req.table.clone() };
    let mut key_type = None;
    for f in &req.filters {
        match (f.column.as_str(), f.op.as_str()) {
            ("key", "=") => pattern = glob_escape(&f.value),
            ("key", "contains") => {
                let prefix = pattern.trim_end_matches('*');
                pattern = format!("{prefix}*{}*", glob_escape(&f.value));
            }
            ("key", "like") => pattern = f.value.replace('%', "*").replace('_', "?"),
            ("type", "=") => key_type = Some(f.value.to_lowercase()),
            (col, op) => bail!("Redis can filter keys by pattern and type only (got {col} {op})"),
        }
    }
    if !req.raw_where.trim().is_empty() {
        pattern = req.raw_where.trim().to_string();
    }
    Ok(KeyQuery { pattern, key_type })
}

impl RedisEngine {
    async fn conn(&self, db: i64) -> Result<MultiplexedConnection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(&db) {
            return Ok(c.clone());
        }
        let info = self
            .addr
            .clone()
            .into_connection_info()?
            .set_redis_settings(self.settings.clone().set_db(db));
        let client = Client::open(info)?;
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| anyhow!("timed out connecting to Redis"))??;
        conns.insert(db, conn.clone());
        Ok(conn)
    }

    /// Up to SCAN_LIMIT matching keys, sorted. The bool says whether the scan stopped early.
    async fn scan(&self, db: i64, q: &KeyQuery) -> Result<(Vec<String>, bool)> {
        let mut conn = self.conn(db).await?;
        let mut cursor: u64 = 0;
        let mut keys = Vec::new();
        loop {
            let mut cmd = redis::cmd("SCAN");
            cmd.arg(cursor).arg("MATCH").arg(&q.pattern).arg("COUNT").arg(1000);
            if let Some(t) = &q.key_type {
                cmd.arg("TYPE").arg(t);
            }
            let (next, batch): (u64, Vec<Vec<u8>>) = cmd.query_async(&mut conn).await?;
            keys.extend(batch.iter().map(|k| String::from_utf8_lossy(k).into_owned()));
            cursor = next;
            if cursor == 0 || keys.len() >= SCAN_LIMIT {
                break;
            }
        }
        let capped = cursor != 0;
        keys.sort();
        keys.dedup();
        Ok((keys, capped))
    }

    /// Reads type, TTL and value of each key.
    async fn describe(&self, db: i64, keys: &[String]) -> Result<Vec<Vec<Cell>>> {
        let mut conn = self.conn(db).await?;
        let mut meta = redis::pipe();
        for k in keys {
            meta.cmd("TYPE").arg(k).cmd("TTL").arg(k);
        }
        let meta: Vec<Value> = meta.query_async(&mut conn).await?;
        let mut fetch = redis::pipe();
        let mut types = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            let t = to_text(&meta[i * 2]).unwrap_or_default();
            match t.as_str() {
                "string" => fetch.cmd("GET").arg(k),
                "hash" => fetch.cmd("HGETALL").arg(k),
                "list" => fetch.cmd("LRANGE").arg(k).arg(0).arg(ELEMENT_LIMIT - 1),
                "set" => fetch.cmd("SMEMBERS").arg(k),
                "zset" => fetch.cmd("ZRANGE").arg(k).arg(0).arg(ELEMENT_LIMIT - 1).arg("WITHSCORES"),
                "stream" => fetch.cmd("XRANGE").arg(k).arg("-").arg("+").arg("COUNT").arg(100),
                _ => fetch.cmd("ECHO").arg(""),
            };
            types.push(t);
        }
        let values: Vec<Value> = if keys.is_empty() { Vec::new() } else { fetch.query_async(&mut conn).await? };
        Ok(keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let ttl = match &meta[i * 2 + 1] {
                    Value::Int(n) if *n >= 0 => Some(n.to_string()),
                    _ => None,
                };
                let value = match types[i].as_str() {
                    "string" => to_text(&values[i]),
                    "hash" => Some(
                        Json::Object(pairs(&values[i]).into_iter().map(|(f, v)| (f, to_json(&v))).collect())
                            .to_string(),
                    ),
                    "zset" => Some(
                        Json::Object(
                            pairs(&values[i])
                                .into_iter()
                                .map(|(m, s)| {
                                    let score = to_text(&s).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                    (m, json!(score))
                                })
                                .collect(),
                        )
                        .to_string(),
                    ),
                    _ => Some(to_json(&values[i]).to_string()),
                };
                vec![Some(k.clone()), Some(types[i].clone()), ttl, value]
            })
            .collect())
    }

    /// Commands that rebuild `key` as `kind` holding `value` (JSON for collections).
    fn write_value(key: &str, kind: &str, value: &str, keep_ttl: bool) -> Result<Vec<Vec<String>>> {
        let parse = || -> Result<Json> {
            serde_json::from_str(value).with_context(|| format!("the {kind} value of {key} must be JSON"))
        };
        let scalar = |v: &Json| match v {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        };
        let mut cmds = Vec::new();
        if kind == "string" {
            let mut set = vec!["SET".into(), key.into(), value.into()];
            if keep_ttl {
                set.push("KEEPTTL".into());
            }
            return Ok(vec![set]);
        }
        cmds.push(vec!["DEL".into(), key.into()]);
        let mut cmd = match kind {
            "hash" => {
                let Json::Object(map) = parse()? else { bail!("a hash value must be a JSON object") };
                let mut c = vec!["HSET".to_string(), key.to_string()];
                for (f, v) in map {
                    c.push(f);
                    c.push(scalar(&v));
                }
                c
            }
            "list" | "set" => {
                let Json::Array(items) = parse()? else { bail!("a {kind} value must be a JSON array") };
                let mut c = vec![if kind == "list" { "RPUSH" } else { "SADD" }.to_string(), key.to_string()];
                c.extend(items.iter().map(scalar));
                c
            }
            "zset" => {
                let Json::Object(map) = parse()? else { bail!("a zset value must be a JSON object of member: score") };
                let mut c = vec!["ZADD".to_string(), key.to_string()];
                for (m, s) in map {
                    c.push(scalar(&s));
                    c.push(m);
                }
                c
            }
            other => bail!("editing {other} values isn't supported"),
        };
        if cmd.len() == 2 {
            // Empty collections don't exist in Redis: the key is simply removed.
            return Ok(cmds);
        }
        cmds.push(std::mem::take(&mut cmd));
        Ok(cmds)
    }

    /// Plans the commands for a change set. TTLs of rewritten collections are
    /// restored from `ttls` (key → seconds).
    fn plan(set: &ChangeSet, current: &HashMap<String, (String, Option<i64>)>) -> Result<Vec<Vec<String>>> {
        let get = |vals: &[CellValue], col: &str| vals.iter().find(|c| c.column == col).map(|c| c.value.clone());
        let mut cmds = Vec::new();
        for change in &set.changes {
            match change {
                RowChange::Delete { key } => {
                    let k = get(key, "key").flatten().context("missing key")?;
                    cmds.push(vec!["DEL".into(), k]);
                }
                RowChange::Insert { values } => {
                    let k = get(values, "key").flatten().context("a new Redis row needs a key")?;
                    let kind = get(values, "type").flatten().unwrap_or_else(|| "string".into());
                    let value = get(values, "value").flatten().unwrap_or_default();
                    cmds.extend(Self::write_value(&k, &kind, &value, false)?);
                    if let Some(Some(ttl)) = get(values, "ttl") {
                        cmds.push(vec!["EXPIRE".into(), k, ttl]);
                    }
                }
                RowChange::Update { key, values } => {
                    let mut k = get(key, "key").flatten().context("missing key")?;
                    let (kind, ttl) = current.get(&k).cloned().with_context(|| format!("key {k} no longer exists"))?;
                    if let Some(new_kind) = get(values, "type").flatten() {
                        if new_kind != kind {
                            bail!("changing the type of {k} isn't supported; delete it and add a new key");
                        }
                    }
                    if let Some(v) = get(values, "value") {
                        cmds.extend(Self::write_value(&k, &kind, &v.unwrap_or_default(), true)?);
                        if kind != "string" {
                            if let Some(t) = ttl {
                                cmds.push(vec!["EXPIRE".into(), k.clone(), t.to_string()]);
                            }
                        }
                    }
                    if let Some(Some(new_key)) = get(values, "key") {
                        cmds.push(vec!["RENAME".into(), k.clone(), new_key.clone()]);
                        k = new_key;
                    }
                    if let Some(t) = get(values, "ttl") {
                        cmds.push(match t {
                            Some(secs) if !secs.trim().is_empty() => vec!["EXPIRE".into(), k, secs.trim().to_string()],
                            _ => vec!["PERSIST".into(), k],
                        });
                    }
                }
            }
        }
        Ok(cmds)
    }

    /// Type and TTL of every key touched by updates.
    async fn current_state(&self, db: i64, set: &ChangeSet) -> Result<HashMap<String, (String, Option<i64>)>> {
        let keys: Vec<String> = set
            .changes
            .iter()
            .filter_map(|c| match c {
                RowChange::Update { key, .. } => key.iter().find(|c| c.column == "key").and_then(|c| c.value.clone()),
                _ => None,
            })
            .collect();
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let mut conn = self.conn(db).await?;
        let mut pipe = redis::pipe();
        for k in &keys {
            pipe.cmd("TYPE").arg(k).cmd("TTL").arg(k);
        }
        let out: Vec<Value> = pipe.query_async(&mut conn).await?;
        Ok(keys
            .into_iter()
            .enumerate()
            .filter_map(|(i, k)| {
                let kind = to_text(&out[i * 2]).unwrap_or_default();
                let ttl = match out[i * 2 + 1] {
                    Value::Int(n) if n >= 0 => Some(n),
                    _ => None,
                };
                (kind != "none").then_some((k, (kind, ttl)))
            })
            .collect())
    }
}

#[async_trait]
impl Driver for RedisEngine {
    async fn query(&self, line: &str) -> Result<Vec<QueryResult>> {
        let started = Instant::now();
        let args = tokenize(line)?;
        let Some(name) = args.first().map(|a| a.to_uppercase()) else { return Ok(Vec::new()) };
        if matches!(name.as_str(), "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR") {
            bail!("{name} streams forever and isn't supported in the query editor");
        }
        if name == "SELECT" {
            let db: i64 = args.get(1).and_then(|d| d.parse().ok()).context("usage: SELECT <db>")?;
            self.conn(db).await?;
            *self.current_db.lock().await = db;
            return Ok(vec![message_result(line, 0)]);
        }
        let db = *self.current_db.lock().await;
        let mut conn = self.conn(db).await?;
        let mut cmd = redis::cmd(&args[0]);
        for a in &args[1..] {
            cmd.arg(a);
        }
        let reply: Value = cmd.query_async(&mut conn).await?;
        let mut result = match &reply {
            Value::Array(items) | Value::Set(items) => {
                if name == "HGETALL" || name == "CONFIG" || (name == "ZRANGE" && line.to_uppercase().contains("WITHSCORES")) {
                    let rows = pairs(&reply).into_iter().map(|(k, v)| vec![Some(k), to_text(&v)]).collect();
                    table_result(line, vec!["field".into(), "value".into()], rows)
                } else {
                    let rows = items
                        .iter()
                        .enumerate()
                        .map(|(i, v)| vec![Some((i + 1).to_string()), to_text(v)])
                        .collect();
                    table_result(line, vec!["#".into(), "value".into()], rows)
                }
            }
            Value::Map(p) => table_result(
                line,
                vec!["field".into(), "value".into()],
                p.iter().map(|(k, v)| vec![to_text(k), to_text(v)]).collect(),
            ),
            other => table_result(line, vec!["result".into()], vec![vec![to_text(other)]]),
        };
        result.duration_ms = started.elapsed().as_millis() as u64;
        Ok(vec![result])
    }

    async fn cancel(&self) -> Result<()> {
        bail!("Redis commands can't be cancelled")
    }

    async fn use_schema(&self, schema: &str) -> Result<()> {
        let db = parse_db(schema)?;
        self.conn(db).await?;
        *self.current_db.lock().await = db;
        Ok(())
    }

    async fn close(&self) {
        self.conns.lock().await.clear();
    }
}

#[async_trait]
impl Browser for RedisEngine {
    async fn list_schemas(&self) -> Result<Vec<String>> {
        Ok((0..self.databases).map(|i| format!("db{i}")).collect())
    }

    /// "*" for all keys, then one entry per prefix before the first ':'.
    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>> {
        let db = parse_db(schema)?;
        let (keys, _) = self.scan(db, &KeyQuery { pattern: "*".into(), key_type: None }).await?;
        let mut prefixes: BTreeMap<String, usize> = BTreeMap::new();
        for k in &keys {
            if let Some((prefix, _)) = k.split_once(':') {
                *prefixes.entry(prefix.to_string()).or_default() += 1;
            }
        }
        let mut out = vec![TableInfo { name: "*".into(), kind: "keyspace".into() }];
        out.extend(
            prefixes
                .into_keys()
                .map(|p| TableInfo { name: format!("{}:*", glob_escape(&p)), kind: "keyspace".into() }),
        );
        Ok(out)
    }

    async fn table_columns(&self, _schema: &str, _table: &str) -> Result<Vec<ColumnInfo>> {
        let col = |name: &str, data_type: &str, pk: bool| ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            nullable: !pk && name != "type",
            default: None,
            is_primary_key: pk,
        };
        Ok(vec![
            col("key", "key", true),
            col("type", "string | hash | list | set | zset | stream", false),
            col("ttl", "seconds (NULL = no expiry)", false),
            col("value", "text, or JSON for collections", false),
        ])
    }

    async fn schema_columns(&self, _schema: &str) -> Result<BTreeMap<String, Vec<String>>> {
        Ok(BTreeMap::new())
    }

    async fn fetch_rows(&self, req: &FetchRequest) -> Result<QueryResult> {
        let started = Instant::now();
        let db = parse_db(&req.schema)?;
        let q = key_query(req)?;
        let (mut keys, capped) = self.scan(db, &q).await?;
        if req.sort.as_ref().is_some_and(|s| s.column == "key" && s.desc) {
            keys.reverse();
        }
        let page: Vec<String> = keys.into_iter().skip(req.offset as usize).take(req.limit as usize).collect();
        let mut rows = self.describe(db, &page).await?;
        if let Some(sort) = req.sort.as_ref().filter(|s| s.column != "key") {
            let idx = ["key", "type", "ttl", "value"].iter().position(|c| *c == sort.column).unwrap_or(0);
            rows.sort_by(|a, b| {
                let o = if idx == 2 {
                    let n = |c: &Cell| c.as_ref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(i64::MAX);
                    n(&a[idx]).cmp(&n(&b[idx]))
                } else {
                    a[idx].cmp(&b[idx])
                };
                if sort.desc { o.reverse() } else { o }
            });
        }
        let mut cmd = format!("SCAN 0 MATCH {}", quote(&q.pattern));
        if let Some(t) = &q.key_type {
            cmd.push_str(&format!(" TYPE {t}"));
        }
        let mut result = table_result(&cmd, vec!["key".into(), "type".into(), "ttl".into(), "value".into()], rows);
        result.duration_ms = started.elapsed().as_millis() as u64;
        if capped {
            result.notice = Some(format!("Showing the first {SCAN_LIMIT} matching keys"));
        }
        Ok(result)
    }

    async fn count_rows(&self, req: &FetchRequest) -> Result<u64> {
        let db = parse_db(&req.schema)?;
        let q = key_query(req)?;
        if q.pattern == "*" && q.key_type.is_none() {
            let mut conn = self.conn(db).await?;
            return Ok(redis::cmd("DBSIZE").query_async(&mut conn).await?);
        }
        Ok(self.scan(db, &q).await?.0.len() as u64)
    }

    async fn preview_changes(&self, set: &ChangeSet) -> Result<Vec<String>> {
        let db = parse_db(&set.schema)?;
        let current = self.current_state(db, set).await?;
        let mut lines = vec![format!("SELECT {db}"), "MULTI".to_string()];
        lines.extend(
            Self::plan(set, &current)?
                .iter()
                .map(|c| c.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")),
        );
        lines.push("EXEC".into());
        Ok(lines)
    }

    async fn commit_changes(&self, set: &ChangeSet) -> Result<u64> {
        let db = parse_db(&set.schema)?;
        let current = self.current_state(db, set).await?;
        let plan = Self::plan(set, &current)?;
        let mut pipe = redis::pipe();
        pipe.atomic();
        for c in &plan {
            let mut cmd = redis::cmd(&c[0]);
            for a in &c[1..] {
                cmd.arg(a);
            }
            pipe.add_command(cmd);
        }
        let mut conn = self.conn(db).await?;
        let replies: Vec<Value> = pipe.query_async(&mut conn).await?;
        if let Some(Value::ServerError(e)) = replies.iter().find(|r| matches!(r, Value::ServerError(_))) {
            bail!("Redis rejected a command: {e:?}");
        }
        Ok(set.changes.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_quoted_arguments() {
        assert_eq!(
            tokenize(r#"SET "user:1 name" 'O"Hara' plain "a\"b\n""#).unwrap(),
            vec!["SET", "user:1 name", "O\"Hara", "plain", "a\"b\n"]
        );
        assert!(tokenize("GET \"open").is_err());
    }

    #[test]
    fn plans_collection_rewrites_and_ttl() {
        let cv = |c: &str, v: Option<&str>| CellValue { column: c.into(), value: v.map(Into::into) };
        let set = ChangeSet {
            schema: "db0".into(),
            table: "*".into(),
            key_is_primary: true,
            changes: vec![
                RowChange::Update { key: vec![cv("key", Some("h"))], values: vec![cv("value", Some(r#"{"a":1,"b":"x"}"#)), cv("key", Some("h2"))] },
                RowChange::Update { key: vec![cv("key", Some("s"))], values: vec![cv("ttl", None)] },
                RowChange::Insert { values: vec![cv("key", Some("z")), cv("type", Some("zset")), cv("value", Some(r#"{"m":2.5}"#)), cv("ttl", Some("60"))] },
                RowChange::Delete { key: vec![cv("key", Some("gone"))] },
            ],
        };
        let current = HashMap::from([
            ("h".to_string(), ("hash".to_string(), Some(30))),
            ("s".to_string(), ("string".to_string(), Some(10))),
        ]);
        let plan: Vec<String> = RedisEngine::plan(&set, &current).unwrap().iter().map(|c| c.join(" ")).collect();
        assert_eq!(
            plan,
            vec![
                "DEL h", "HSET h a 1 b x", "EXPIRE h 30", "RENAME h h2",
                "PERSIST s",
                "DEL z", "ZADD z 2.5 m", "EXPIRE z 60",
                "DEL gone",
            ]
        );
    }
}
