//! MongoDB: databases are schemas, collections are tables and documents are
//! rows whose columns are the top-level fields. Values that aren't strings are
//! shown in shell syntax (`ObjectId("…")`, `ISODate("…")`) or relaxed Extended
//! JSON, and parsed back using the field's sampled type. The console accepts
//! `db.coll.find({...})`-style calls with JSON5 arguments.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use mongodb::bson::{self, doc, Bson, Document};
use mongodb::options::ClientOptions;
use mongodb::results::CollectionType;
use mongodb::Client;
use regex::Regex;
use serde_json::Value as Json;
use tokio::sync::Mutex;

use super::{message_result, table_result, Browser, Driver, Engine, MAX_RESULT_ROWS};
use crate::model::*;

/// Documents sampled to infer a collection's fields.
const SAMPLE_SIZE: i64 = 200;

pub struct MongoEngine {
    client: Client,
    current_db: Mutex<String>,
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The connection string for `cfg`: the given URI (with `<password>` filled in)
/// or one built from host, port and credentials.
pub fn connection_uri(cfg: &ConnectionConfig, tunnel: Option<(IpAddr, u16)>) -> Result<String> {
    let password = cfg.password.as_deref().unwrap_or_default();
    if !cfg.uri.trim().is_empty() {
        if tunnel.is_some() {
            bail!("an SSH tunnel needs host and port instead of a connection string");
        }
        return Ok(cfg
            .uri
            .trim()
            .replace("<db_password>", &percent_encode(password))
            .replace("<password>", &percent_encode(password)));
    }
    let (host, port) = match tunnel {
        Some((ip, p)) => (ip.to_string(), p),
        None => (
            if cfg.host.is_empty() { "127.0.0.1".into() } else { cfg.host.clone() },
            if cfg.port == 0 { 27017 } else { cfg.port },
        ),
    };
    let auth = if cfg.user.is_empty() {
        String::new()
    } else {
        format!("{}:{}@", percent_encode(&cfg.user), percent_encode(password))
    };
    let mut params = vec!["directConnection=true".to_string()];
    if !cfg.user.is_empty() {
        params.push("authSource=admin".into());
    }
    match cfg.ssl_mode {
        SslMode::Disable | SslMode::Prefer => {}
        SslMode::Require => params.push("tls=true&tlsAllowInvalidCertificates=true".into()),
        SslMode::VerifyFull => params.push("tls=true".into()),
    }
    Ok(format!("mongodb://{auth}{host}:{port}/{}?{}", cfg.database, params.join("&")))
}

pub async fn connect(cfg: &ConnectionConfig, tunnel: Option<(IpAddr, u16)>) -> Result<Engine> {
    let mut options = ClientOptions::parse(connection_uri(cfg, tunnel)?).await?;
    options.app_name = Some("HuginnTables".into());
    options.server_selection_timeout = Some(Duration::from_secs(10));
    options.connect_timeout = Some(Duration::from_secs(10));
    let default_db = options
        .default_database
        .clone()
        .or_else(|| Some(cfg.database.clone()).filter(|d| !d.is_empty()))
        .unwrap_or_else(|| "test".into());
    let client = Client::with_options(options)?;
    let build = client.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
    let version = build.get_str("version").unwrap_or("?");
    let engine = Arc::new(MongoEngine { client, current_db: Mutex::new(default_db.clone()) });
    Ok(Engine {
        driver: engine.clone(),
        browser: engine,
        server_version: format!("MongoDB {version}"),
        database: default_db.clone(),
        schema: default_db,
        read_only: false,
    })
}

// ---- value conversion -------------------------------------------------

pub fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null | Bson::Undefined => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Decimal128(_) => "decimal",
        _ => "other",
    }
}

pub fn render(b: &Bson) -> Cell {
    Some(match b {
        Bson::Null | Bson::Undefined => return None,
        Bson::String(s) => s.clone(),
        Bson::ObjectId(o) => format!("ObjectId(\"{o}\")"),
        Bson::DateTime(d) => format!("ISODate(\"{}\")", d.try_to_rfc3339_string().unwrap_or_else(|_| d.to_string())),
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(f) => f.to_string(),
        Bson::Boolean(v) => v.to_string(),
        other => other.clone().into_relaxed_extjson().to_string(),
    })
}

static SHELL_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:new\s+)?(ObjectId|ISODate|Date|NumberDecimal|NumberLong|NumberInt)\(\s*["']([^"']*)["']\s*\)"#).unwrap()
});

/// Rewrites shell helpers like `ObjectId("…")` into Extended JSON.
fn shell_to_extjson(text: &str) -> String {
    SHELL_LITERAL
        .replace_all(text, |c: &regex::Captures| {
            let v = &c[2];
            match &c[1] {
                "ObjectId" => format!(r#"{{"$oid":"{v}"}}"#),
                "ISODate" | "Date" => format!(r#"{{"$date":"{v}"}}"#),
                "NumberDecimal" => format!(r#"{{"$numberDecimal":"{v}"}}"#),
                "NumberLong" => format!(r#"{{"$numberLong":"{v}"}}"#),
                _ => format!(r#"{{"$numberInt":"{v}"}}"#),
            }
        })
        .into_owned()
}

/// Parses JSON5 (unquoted keys, single quotes) with shell helpers into BSON.
pub fn parse_json5(text: &str) -> Result<Bson> {
    let source = shell_to_extjson(text);
    let json: Json = match json5::from_str(&source) {
        Ok(j) => j,
        // macOS "smart quotes" turn ' and " into typographic quotes while typing.
        Err(e) => json5::from_str(&straighten_quotes(&source)).map_err(|_| anyhow!("could not parse {text}: {e}"))?,
    };
    Bson::try_from(json).map_err(|e| anyhow!("invalid Extended JSON: {e}"))
}

fn straighten_quotes(s: &str) -> String {
    s.replace(['\u{2018}', '\u{2019}'], "'").replace(['\u{201C}', '\u{201D}'], "\"")
}

/// Parses a grid cell back into BSON, guided by the field's sampled type.
pub fn parse_cell(text: &Cell, hint: Option<&str>) -> Result<Bson> {
    let Some(text) = text else { return Ok(Bson::Null) };
    let t = text.trim();
    let whole = SHELL_LITERAL.find(t).is_some_and(|m| m.start() == 0 && m.end() == t.len());
    if whole {
        return parse_json5(t);
    }
    Ok(match hint {
        Some("string") => Bson::String(text.clone()),
        Some("int") => t.parse::<i32>().map(Bson::Int32).or_else(|_| t.parse::<i64>().map(Bson::Int64)).with_context(|| format!("{t} is not an integer"))?,
        Some("long") => Bson::Int64(t.parse().with_context(|| format!("{t} is not an integer"))?),
        Some("double") => Bson::Double(t.parse().with_context(|| format!("{t} is not a number"))?),
        Some("bool") => Bson::Boolean(t.parse().with_context(|| format!("{t} is not true/false"))?),
        Some("decimal") => Bson::Decimal128(bson::Decimal128::from_str(t).map_err(|e| anyhow!("{t} is not a decimal: {e}"))?),
        Some("date") => Bson::DateTime(bson::DateTime::parse_rfc3339_str(t).with_context(|| format!("{t} is not an ISO date"))?),
        Some("object") | Some("array") => parse_json5(t)?,
        // Unknown or mixed types: JSON when it parses, text otherwise.
        _ => match json5::from_str::<Json>(&shell_to_extjson(t)) {
            Ok(Json::String(_)) | Err(_) => match parse_json5(t) {
                // Typed with smart quotes but meant as an object/array.
                Ok(b @ (Bson::Document(_) | Bson::Array(_))) => b,
                _ => Bson::String(text.clone()),
            },
            Ok(j) => Bson::try_from(j).unwrap_or_else(|_| Bson::String(text.clone())),
        },
    })
}

fn to_document(b: Bson, what: &str) -> Result<Document> {
    match b {
        Bson::Document(d) => Ok(d),
        _ => bail!("{what} must be an object"),
    }
}

/// Field names in first-seen order, `_id` first, with each field's most common type.
fn infer_fields(docs: &[Document]) -> Vec<(String, String)> {
    let mut order: Vec<String> = vec!["_id".into()];
    let mut types: HashMap<String, HashMap<&'static str, usize>> = HashMap::new();
    for d in docs {
        for (k, v) in d {
            if !order.contains(k) {
                order.push(k.clone());
            }
            if !matches!(v, Bson::Null) {
                *types.entry(k.clone()).or_default().entry(type_name(v)).or_default() += 1;
            }
        }
    }
    order
        .into_iter()
        .map(|k| {
            let t = types
                .get(&k)
                .and_then(|m| m.iter().max_by_key(|(_, n)| **n).map(|(t, _)| t.to_string()))
                .unwrap_or_else(|| if k == "_id" { "objectId".into() } else { "null".into() });
            (k, t)
        })
        .collect()
}

fn docs_result(source: &str, docs: &[Document], known: &[String]) -> QueryResult {
    let mut columns: Vec<String> = known.to_vec();
    for d in docs {
        for k in d.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    if columns.is_empty() {
        columns.push("_id".into());
    }
    let rows = docs
        .iter()
        .map(|d| columns.iter().map(|c| d.get(c).and_then(render)).collect())
        .collect();
    table_result(source, columns, rows)
}

fn filter_doc(req: &FetchRequest, hints: &HashMap<String, String>) -> Result<Document> {
    let mut filter = Document::new();
    for f in &req.filters {
        let hint = hints.get(&f.column).map(String::as_str);
        let value = || parse_cell(&Some(f.value.clone()), hint);
        let regex = |pattern: String| Bson::RegularExpression(bson::Regex { pattern, options: "i".into() });
        let cond = match f.op.as_str() {
            "=" => value()?,
            "<>" => Bson::Document(doc! { "$ne": value()? }),
            "<" => Bson::Document(doc! { "$lt": value()? }),
            ">" => Bson::Document(doc! { "$gt": value()? }),
            "<=" => Bson::Document(doc! { "$lte": value()? }),
            ">=" => Bson::Document(doc! { "$gte": value()? }),
            "contains" => regex(regex::escape(&f.value)),
            "notContains" => Bson::Document(doc! { "$not": regex(regex::escape(&f.value)) }),
            "like" => regex(format!(
                "^{}$",
                regex::escape(&f.value).replace('%', ".*").replace('_', ".")
            )),
            "in" => Bson::Document(doc! { "$in": f.value.split(',').map(|v| parse_cell(&Some(v.trim().to_string()), hint)).collect::<Result<Vec<_>>>()? }),
            "isNull" => Bson::Null,
            "isNotNull" => Bson::Document(doc! { "$ne": Bson::Null }),
            other => bail!("unknown filter operator {other}"),
        };
        filter.insert(f.column.clone(), cond);
    }
    if !req.raw_where.trim().is_empty() {
        let raw = to_document(parse_json5(req.raw_where.trim())?, "the filter")?;
        filter = if filter.is_empty() { raw } else { doc! { "$and": [filter, raw] } };
    }
    Ok(filter)
}

fn ext(b: &Bson) -> String {
    b.clone().into_relaxed_extjson().to_string()
}

impl MongoEngine {
    async fn sample(&self, db: &str, coll: &str) -> Result<Vec<Document>> {
        let c = self.client.database(db).collection::<Document>(coll);
        Ok(c.find(doc! {}).limit(SAMPLE_SIZE).await?.try_collect().await?)
    }

    async fn hints(&self, db: &str, coll: &str) -> Result<HashMap<String, String>> {
        Ok(infer_fields(&self.sample(db, coll).await?).into_iter().collect())
    }

    /// (filter, update/document) pairs for each change, ready to preview or run.
    fn plan(set: &ChangeSet, hints: &HashMap<String, String>) -> Result<Vec<(String, Document, Option<Document>)>> {
        let hint = |c: &str| hints.get(c).map(String::as_str);
        let id_filter = |key: &[CellValue]| -> Result<Document> {
            let mut f = Document::new();
            for k in key {
                f.insert(k.column.clone(), parse_cell(&k.value, hint(&k.column))?);
            }
            Ok(f)
        };
        let mut out = Vec::new();
        for change in &set.changes {
            match change {
                RowChange::Insert { values } => {
                    let mut d = Document::new();
                    for v in values {
                        d.insert(v.column.clone(), parse_cell(&v.value, hint(&v.column))?);
                    }
                    out.push(("insertOne".into(), d, None));
                }
                RowChange::Update { key, values } => {
                    let mut set_doc = Document::new();
                    for v in values {
                        set_doc.insert(v.column.clone(), parse_cell(&v.value, hint(&v.column))?);
                    }
                    out.push(("updateOne".into(), id_filter(key)?, Some(doc! { "$set": set_doc })));
                }
                RowChange::Delete { key } => out.push(("deleteOne".into(), id_filter(key)?, None)),
            }
        }
        Ok(out)
    }

    async fn run_shell(&self, line: &str) -> Result<QueryResult> {
        let text = line.trim().trim_end_matches(';').trim();
        let mut db_name = self.current_db.lock().await.clone();

        if let Some(rest) = text.strip_prefix("use ") {
            let name = rest.trim().to_string();
            *self.current_db.lock().await = name.clone();
            return Ok(QueryResult { notice: Some(format!("switched to db {name}")), ..message_result(line, 0) });
        }
        match text {
            "show dbs" | "show databases" => {
                let names = self.client.list_database_names().await?;
                return Ok(table_result(line, vec!["database".into()], names.into_iter().map(|n| vec![Some(n)]).collect()));
            }
            "show collections" | "show tables" => {
                let names = self.client.database(&db_name).list_collection_names().await?;
                return Ok(table_result(line, vec!["collection".into()], names.into_iter().map(|n| vec![Some(n)]).collect()));
            }
            _ => {}
        }
        if text.starts_with('{') {
            let cmd = to_document(parse_json5(text)?, "a command")?;
            let reply = self.client.database(&db_name).run_command(cmd).await?;
            return Ok(command_reply(line, reply));
        }

        let call = parse_call(text)?;
        if let Some(name) = &call.database_override {
            db_name = name.clone();
        }
        let db = self.client.database(&db_name);
        let arg = |i: usize| call.args.get(i).cloned();
        let doc_arg = |i: usize, what: &str| -> Result<Document> {
            match arg(i) {
                None => Ok(Document::new()),
                Some(b) => to_document(b, what),
            }
        };
        let Some(coll_name) = call.collection.clone() else {
            return match call.method.as_str() {
                "runCommand" | "adminCommand" => {
                    let target = if call.method == "adminCommand" { self.client.database("admin") } else { db };
                    let reply = target.run_command(doc_arg(0, "the command")?).await?;
                    Ok(command_reply(line, reply))
                }
                "getCollectionNames" => {
                    let names = db.list_collection_names().await?;
                    Ok(table_result(line, vec!["collection".into()], names.into_iter().map(|n| vec![Some(n)]).collect()))
                }
                other => bail!("db.{other}() isn't supported; use db.<collection>.<method>(…) or db.runCommand({{…}})"),
            };
        };
        let coll = db.collection::<Document>(&coll_name);
        let chained = |name: &str| call.chain.iter().find(|(n, _)| n == name).and_then(|(_, a)| a.first().cloned());
        let as_i64 = |b: Option<Bson>| b.and_then(|b| match b {
            Bson::Int32(i) => Some(i as i64),
            Bson::Int64(i) => Some(i),
            Bson::Double(f) => Some(f as i64),
            _ => None,
        });
        let affected = |n: u64| Ok(message_result(line, n));

        match call.method.as_str() {
            "find" | "findOne" => {
                let mut find = coll.find(doc_arg(0, "the filter")?);
                if let Some(Bson::Document(p)) = arg(1) {
                    find = find.projection(p);
                }
                if let Some(Bson::Document(s)) = chained("sort") {
                    find = find.sort(s);
                }
                if let Some(n) = as_i64(chained("skip")) {
                    find = find.skip(n as u64);
                }
                let limit = if call.method == "findOne" { 1 } else { as_i64(chained("limit")).unwrap_or(MAX_RESULT_ROWS as i64 + 1) };
                let docs: Vec<Document> = find.limit(limit).await?.try_collect().await?;
                Ok(docs_result(line, &docs, &[]))
            }
            "aggregate" => {
                let pipeline = match arg(0) {
                    Some(Bson::Array(stages)) => stages.into_iter().map(|s| to_document(s, "a pipeline stage")).collect::<Result<Vec<_>>>()?,
                    _ => bail!("aggregate() takes an array of stages"),
                };
                let docs: Vec<Document> = coll.aggregate(pipeline).await?.try_collect().await?;
                Ok(docs_result(line, &docs, &[]))
            }
            "countDocuments" | "count" => {
                let n = coll.count_documents(doc_arg(0, "the filter")?).await?;
                Ok(table_result(line, vec!["count".into()], vec![vec![Some(n.to_string())]]))
            }
            "estimatedDocumentCount" => {
                let n = coll.estimated_document_count().await?;
                Ok(table_result(line, vec!["count".into()], vec![vec![Some(n.to_string())]]))
            }
            "distinct" => {
                let Some(Bson::String(field)) = arg(0) else { bail!("distinct() needs a field name") };
                let values = coll.distinct(field.clone(), doc_arg(1, "the filter")?).await?;
                Ok(table_result(line, vec![field], values.iter().map(|v| vec![render(v)]).collect()))
            }
            "insertOne" => {
                let r = coll.insert_one(doc_arg(0, "the document")?).await?;
                Ok(QueryResult { notice: Some(format!("insertedId: {}", ext(&r.inserted_id))), ..message_result(line, 1) })
            }
            "insertMany" => {
                let Some(Bson::Array(items)) = arg(0) else { bail!("insertMany() takes an array of documents") };
                let docs = items.into_iter().map(|d| to_document(d, "each document")).collect::<Result<Vec<_>>>()?;
                let r = coll.insert_many(docs).await?;
                affected(r.inserted_ids.len() as u64)
            }
            "updateOne" | "updateMany" | "replaceOne" => {
                let filter = doc_arg(0, "the filter")?;
                let update = doc_arg(1, "the update")?;
                let r = match call.method.as_str() {
                    "updateOne" => coll.update_one(filter, update).await?,
                    "updateMany" => coll.update_many(filter, update).await?,
                    _ => coll.replace_one(filter, update).await?,
                };
                Ok(QueryResult {
                    notice: Some(format!("matched {}, modified {}", r.matched_count, r.modified_count)),
                    ..message_result(line, r.modified_count)
                })
            }
            "deleteOne" => affected(coll.delete_one(doc_arg(0, "the filter")?).await?.deleted_count),
            "deleteMany" => affected(coll.delete_many(doc_arg(0, "the filter")?).await?.deleted_count),
            "drop" => {
                coll.drop().await?;
                affected(0)
            }
            "getIndexes" => {
                let reply = db.run_command(doc! { "listIndexes": &coll_name }).await?;
                Ok(command_reply(line, reply))
            }
            "createIndex" => {
                let keys = doc_arg(0, "the index keys")?;
                let mut index = doc! { "key": keys.clone(), "name": keys.keys().map(|k| format!("{k}_{}", keys.get(k).map(ext).unwrap_or_default())).collect::<Vec<_>>().join("_") };
                if let Some(Bson::Document(opts)) = arg(1) {
                    index.extend(opts);
                }
                let reply = db.run_command(doc! { "createIndexes": &coll_name, "indexes": [index] }).await?;
                Ok(command_reply(line, reply))
            }
            other => bail!("{other}() isn't supported in the query editor"),
        }
    }
}

/// Shows a command reply: the documents of a cursor, or the reply itself.
fn command_reply(line: &str, reply: Document) -> QueryResult {
    if let Ok(cursor) = reply.get_document("cursor") {
        if let Ok(batch) = cursor.get_array("firstBatch") {
            let docs: Vec<Document> = batch.iter().filter_map(|b| b.as_document().cloned()).collect();
            return docs_result(line, &docs, &[]);
        }
    }
    docs_result(line, &[reply], &[])
}

#[derive(Debug, PartialEq)]
pub struct ShellCall {
    pub database_override: Option<String>,
    pub collection: Option<String>,
    pub method: String,
    pub args: Vec<Bson>,
    pub chain: Vec<(String, Vec<Bson>)>,
}

/// Reads a parenthesised argument list starting at `s[0] == '('`; returns the
/// inner text and the rest after the closing parenthesis.
fn take_parens(s: &str) -> Result<(&str, &str)> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&s[1..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    bail!("unbalanced parentheses")
}

fn parse_args(inner: &str) -> Result<Vec<Bson>> {
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    match parse_json5(&format!("[{inner}]"))? {
        Bson::Array(items) => Ok(items),
        _ => unreachable!(),
    }
}

/// Parses `db.coll.method(args).chain(args)…`, `db.getCollection("c")…`,
/// `db.getSiblingDB("d").coll…` and `db.runCommand(…)`.
pub fn parse_call(text: &str) -> Result<ShellCall> {
    let mut rest = text.strip_prefix("db.").context("expected db.<collection>.<method>(…), use <db>, show collections, or a {command}")?;
    let mut database_override = None;
    let mut collection = None;
    if let Some(after) = rest.strip_prefix("getSiblingDB") {
        let (inner, tail) = take_parens(after)?;
        match parse_args(inner)?.first() {
            Some(Bson::String(d)) => database_override = Some(d.clone()),
            _ => bail!("getSiblingDB() needs a database name"),
        }
        rest = tail.strip_prefix('.').context("expected .<collection> after getSiblingDB()")?;
    }
    if let Some(after) = rest.strip_prefix("getCollection") {
        let (inner, tail) = take_parens(after)?;
        match parse_args(inner)?.first() {
            Some(Bson::String(c)) => collection = Some(c.clone()),
            _ => bail!("getCollection() needs a collection name"),
        }
        rest = tail.strip_prefix('.').context("expected .<method>(…) after getCollection()")?;
    }
    let paren = rest.find('(').context("expected a method call like .find()")?;
    let (target, after) = rest.split_at(paren);
    let method;
    if collection.is_some() {
        method = target.to_string();
    } else {
        match target.rsplit_once('.') {
            Some((c, m)) => {
                collection = Some(c.to_string());
                method = m.to_string();
            }
            None => method = target.to_string(),
        }
    }
    let (inner, mut tail) = take_parens(after)?;
    let args = parse_args(inner)?;
    let mut chain = Vec::new();
    loop {
        tail = tail.trim_start();
        if tail.is_empty() {
            break;
        }
        let t = tail.strip_prefix('.').with_context(|| format!("unexpected text: {tail}"))?;
        let paren = t.find('(').with_context(|| format!("expected a call: {t}"))?;
        let name = t[..paren].trim().to_string();
        let (inner, next) = take_parens(&t[paren..])?;
        chain.push((name, parse_args(inner)?));
        tail = next;
    }
    Ok(ShellCall { database_override, collection, method, args, chain })
}

#[async_trait]
impl Driver for MongoEngine {
    async fn query(&self, line: &str) -> Result<Vec<QueryResult>> {
        let started = Instant::now();
        let mut result = self.run_shell(line).await?;
        result.duration_ms = started.elapsed().as_millis() as u64;
        Ok(vec![result])
    }

    async fn cancel(&self) -> Result<()> {
        bail!("cancelling MongoDB operations isn't supported yet")
    }

    async fn use_schema(&self, schema: &str) -> Result<()> {
        *self.current_db.lock().await = schema.to_string();
        Ok(())
    }

    async fn close(&self) {
        self.client.clone().shutdown().await;
    }
}

#[async_trait]
impl Browser for MongoEngine {
    async fn list_schemas(&self) -> Result<Vec<String>> {
        match self.client.list_database_names().await {
            Ok(names) => Ok(names),
            // Users limited to one database may not list the others.
            Err(_) => Ok(vec![self.current_db.lock().await.clone()]),
        }
    }

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>> {
        let specs: Vec<_> = self.client.database(schema).list_collections().await?.try_collect().await?;
        let mut out: Vec<TableInfo> = specs
            .into_iter()
            .filter(|s| !s.name.starts_with("system."))
            .map(|s| TableInfo {
                kind: if matches!(s.collection_type, CollectionType::View) { "view" } else { "collection" }.into(),
                name: s.name,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn table_columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        Ok(infer_fields(&self.sample(schema, table).await?)
            .into_iter()
            .map(|(name, data_type)| ColumnInfo {
                is_primary_key: name == "_id",
                nullable: name != "_id",
                default: None,
                name,
                data_type,
            })
            .collect())
    }

    async fn schema_columns(&self, schema: &str) -> Result<BTreeMap<String, Vec<String>>> {
        let names = self.client.database(schema).list_collection_names().await?;
        Ok(names.into_iter().map(|n| (n, Vec::new())).collect())
    }

    async fn fetch_rows(&self, req: &FetchRequest) -> Result<QueryResult> {
        let started = Instant::now();
        let fields = infer_fields(&self.sample(&req.schema, &req.table).await?);
        let hints: HashMap<String, String> = fields.iter().cloned().collect();
        let filter = filter_doc(req, &hints)?;
        let mut sort = Document::new();
        if let Some(s) = &req.sort {
            sort.insert(s.column.clone(), if s.desc { -1 } else { 1 });
        }
        if !sort.contains_key("_id") {
            sort.insert("_id", 1);
        }
        let coll = self.client.database(&req.schema).collection::<Document>(&req.table);
        let docs: Vec<Document> = coll
            .find(filter.clone())
            .sort(sort.clone())
            .skip(req.offset)
            .limit(req.limit as i64)
            .await?
            .try_collect()
            .await?;
        let known: Vec<String> = fields.into_iter().map(|(n, _)| n).collect();
        let source = format!(
            "db.getCollection({}).find({}).sort({}).skip({}).limit({})",
            Json::String(req.table.clone()),
            ext(&Bson::Document(filter)),
            ext(&Bson::Document(sort)),
            req.offset,
            req.limit
        );
        let mut result = docs_result(&source, &docs, &known);
        result.duration_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }

    async fn count_rows(&self, req: &FetchRequest) -> Result<u64> {
        let coll = self.client.database(&req.schema).collection::<Document>(&req.table);
        if req.filters.is_empty() && req.raw_where.trim().is_empty() {
            return Ok(coll.estimated_document_count().await?);
        }
        let hints = self.hints(&req.schema, &req.table).await?;
        Ok(coll.count_documents(filter_doc(req, &hints)?).await?)
    }

    async fn preview_changes(&self, set: &ChangeSet) -> Result<Vec<String>> {
        let hints = self.hints(&set.schema, &set.table).await?;
        let coll = Json::String(set.table.clone());
        let mut lines: Vec<String> = Self::plan(set, &hints)?
            .into_iter()
            .map(|(op, a, b)| match b {
                Some(update) => format!("db.getCollection({coll}).{op}({}, {})", ext(&Bson::Document(a)), ext(&Bson::Document(update))),
                None => format!("db.getCollection({coll}).{op}({})", ext(&Bson::Document(a))),
            })
            .collect();
        lines.insert(0, format!("// database {}; changes run one by one (not in a transaction)", set.schema));
        Ok(lines)
    }

    async fn commit_changes(&self, set: &ChangeSet) -> Result<u64> {
        let hints = self.hints(&set.schema, &set.table).await?;
        let plan = Self::plan(set, &hints)?;
        let coll = self.client.database(&set.schema).collection::<Document>(&set.table);
        let mut applied = 0;
        for (i, (op, a, b)) in plan.into_iter().enumerate() {
            let outcome: Result<()> = async {
                match op.as_str() {
                    "insertOne" => {
                        coll.insert_one(a).await?;
                    }
                    "updateOne" => {
                        let r = coll.update_one(a, b.unwrap_or_default()).await?;
                        if r.matched_count == 0 {
                            bail!("the document no longer exists");
                        }
                    }
                    _ => {
                        coll.delete_one(a).await?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                return Err(e.context(format!("change {} of {} failed; {applied} earlier change(s) were saved", i + 1, set.changes.len())));
            }
            applied += 1;
        }
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::oid::ObjectId;

    #[test]
    fn parses_shell_calls() {
        let c = parse_call(r#"db.users.find({age: {$gt: 30}, _id: ObjectId("65a0c0ffee00000000000001")}).sort({name: -1}).limit(5)"#).unwrap();
        assert_eq!(c.collection.as_deref(), Some("users"));
        assert_eq!(c.method, "find");
        let filter = c.args[0].as_document().unwrap();
        assert_eq!(filter.get_document("age").unwrap().get_i32("$gt").unwrap(), 30);
        assert!(matches!(filter.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(c.chain.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["sort", "limit"]);

        let c = parse_call(r#"db.getCollection("weird.name").updateOne({'a': 'x)y'}, {$set: {b: ISODate("2026-01-02T03:04:05Z")}})"#).unwrap();
        assert_eq!(c.collection.as_deref(), Some("weird.name"));
        assert_eq!(c.args.len(), 2);

        let c = parse_call("db.runCommand({ping: 1})").unwrap();
        assert_eq!((c.collection, c.method.as_str()), (None, "runCommand"));
        assert!(parse_call("db.users.find(").is_err());
    }

    #[test]
    fn round_trips_cells_by_type() {
        let oid = ObjectId::parse_str("65a0c0ffee00000000000001").unwrap();
        let shown = render(&Bson::ObjectId(oid)).unwrap();
        assert_eq!(parse_cell(&Some(shown), Some("objectId")).unwrap(), Bson::ObjectId(oid));
        assert_eq!(parse_cell(&Some("42".into()), Some("string")).unwrap(), Bson::String("42".into()));
        assert_eq!(parse_cell(&Some("42".into()), Some("int")).unwrap(), Bson::Int32(42));
        assert_eq!(parse_cell(&Some("2.5".into()), Some("double")).unwrap(), Bson::Double(2.5));
        assert_eq!(parse_cell(&Some("{a: [1, 'x']}".into()), Some("object")).unwrap(), Bson::Document(doc! { "a": [1, "x"] }));
        assert_eq!(parse_cell(&Some("hello".into()), None).unwrap(), Bson::String("hello".into()));
        assert_eq!(
            parse_cell(&Some("{city: 'Bergen', zip: 5003}".into()), Some("object")).unwrap(),
            Bson::Document(doc! { "city": "Bergen", "zip": 5003 })
        );
        // macOS text fields may turn quotes into typographic ones.
        assert_eq!(
            parse_cell(&Some("{city: \u{2018}Bergen\u{2019}, note: \u{201C}hi\u{201D}}".into()), Some("object")).unwrap(),
            Bson::Document(doc! { "city": "Bergen", "note": "hi" })
        );
        assert_eq!(parse_cell(&Some("true".into()), None).unwrap(), Bson::Boolean(true));
        assert_eq!(parse_cell(&None, Some("int")).unwrap(), Bson::Null);
        let date = render(&Bson::DateTime(bson::DateTime::from_millis(0))).unwrap();
        assert_eq!(date, r#"ISODate("1970-01-01T00:00:00Z")"#);
        assert_eq!(parse_cell(&Some(date), Some("date")).unwrap(), Bson::DateTime(bson::DateTime::from_millis(0)));
    }

    #[test]
    fn builds_connection_uris() {
        let mut cfg = ConnectionConfig {
            id: String::new(), name: String::new(), driver: DriverKind::Mongodb, color: String::new(),
            template: String::new(), host: "db.local".into(), port: 0, user: "app".into(), database: "shop".into(),
            ssl_mode: SslMode::Require, file_path: String::new(), uri: String::new(), ssh: SshConfig::default(),
            password: Some("p@ss word".into()), ssh_password: None, ssh_passphrase: None,
        };
        assert_eq!(
            connection_uri(&cfg, None).unwrap(),
            "mongodb://app:p%40ss%20word@db.local:27017/shop?directConnection=true&authSource=admin&tls=true&tlsAllowInvalidCertificates=true"
        );
        cfg.uri = "mongodb+srv://app:<password>@cluster0.example.mongodb.net/?retryWrites=true".into();
        assert_eq!(
            connection_uri(&cfg, None).unwrap(),
            "mongodb+srv://app:p%40ss%20word@cluster0.example.mongodb.net/?retryWrites=true"
        );
        assert!(connection_uri(&cfg, Some(("127.0.0.1".parse().unwrap(), 1))).is_err());
    }
}
