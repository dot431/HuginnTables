//! BigQuery over its REST API. The project holds datasets (schemas) and tables.
//! Browsing an unfiltered table uses `tabledata.list`, which is free; filtering,
//! sorting and the query editor run GoogleSQL jobs. The grid is read-only.
//!
//! Credentials: a service-account key file, or gcloud's application default
//! credentials when no file is given. A custom API endpoint (the emulator)
//! without credentials connects anonymously.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value as Json};
use tokio::sync::Mutex;

use super::{Browser, Driver, Engine, MAX_RESULT_ROWS};
use crate::model::*;

const DEFAULT_ENDPOINT: &str = "https://bigquery.googleapis.com";
const SCOPE: &str = "https://www.googleapis.com/auth/bigquery";

#[derive(Deserialize, Clone)]
struct CredentialFile {
    #[serde(rename = "type")]
    kind: String,
    client_email: Option<String>,
    private_key: Option<String>,
    token_uri: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
}

enum Auth {
    Anonymous,
    Credentials(CredentialFile),
}

struct Token {
    value: String,
    expires: Instant,
}

pub struct BigQueryEngine {
    http: reqwest::Client,
    endpoint: String,
    project: String,
    auth: Auth,
    token: Mutex<Option<Token>>,
    running_job: Mutex<Option<(String, Option<String>)>>,
}

#[derive(Deserialize, Clone, Debug)]
struct Field {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    fields: Vec<Field>,
}

fn adc_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS") {
        return Some(PathBuf::from(p));
    }
    let p = dirs::home_dir()?.join(".config/gcloud/application_default_credentials.json");
    p.exists().then_some(p)
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

pub async fn connect(cfg: &ConnectionConfig) -> Result<Engine> {
    let project = cfg.database.trim().to_string();
    if project.is_empty() {
        bail!("enter the Google Cloud project ID");
    }
    let endpoint = if cfg.host.trim().is_empty() {
        DEFAULT_ENDPOINT.to_string()
    } else {
        cfg.host.trim().trim_end_matches('/').to_string()
    };
    let custom_endpoint = endpoint != DEFAULT_ENDPOINT;
    // gcloud's own credentials are only ever sent to Google, never to a custom endpoint.
    let key_path = match cfg.file_path.trim() {
        "" if custom_endpoint => None,
        "" => adc_path(),
        path => Some(expand_home(path)),
    };
    let auth = match key_path {
        Some(path) => {
            let text = std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
            let creds: CredentialFile = serde_json::from_str(&text).with_context(|| format!("{} is not a Google credentials file", path.display()))?;
            if creds.kind != "service_account" && creds.kind != "authorized_user" {
                bail!("unsupported credential type {:?} in {}", creds.kind, path.display());
            }
            Auth::Credentials(creds)
        }
        None if custom_endpoint => Auth::Anonymous,
        None => bail!(
            "no credentials: choose a service-account key file, or run `gcloud auth application-default login`"
        ),
    };
    let engine = Arc::new(BigQueryEngine {
        http: reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?,
        endpoint,
        project: project.clone(),
        auth,
        token: Mutex::new(None),
        running_job: Mutex::new(None),
    });
    // Validates credentials and project access.
    engine.get(&format!("/projects/{project}/datasets"), &[("maxResults", "1")]).await?;
    Ok(Engine {
        driver: engine.clone(),
        browser: engine,
        server_version: format!("BigQuery · {project}"),
        database: project,
        schema: String::new(),
        read_only: true,
    })
}

/// GoogleSQL identifier quoting.
pub fn ident(name: &str) -> String {
    format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
}

/// GoogleSQL string literal.
pub fn string_literal(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// A literal of the column's type, since BigQuery doesn't coerce strings.
pub fn typed_literal(value: &str, kind: &str) -> Result<String> {
    let v = value.trim();
    Ok(match kind {
        "INTEGER" | "INT64" => {
            v.parse::<i64>().with_context(|| format!("{v} is not an integer"))?;
            v.to_string()
        }
        "FLOAT" | "FLOAT64" | "NUMERIC" | "BIGNUMERIC" => {
            v.parse::<f64>().with_context(|| format!("{v} is not a number"))?;
            v.to_string()
        }
        "BOOLEAN" | "BOOL" => match v.to_lowercase().as_str() {
            "true" => "TRUE".into(),
            "false" => "FALSE".into(),
            _ => bail!("{v} is not true/false"),
        },
        "DATE" | "DATETIME" | "TIMESTAMP" | "TIME" => format!("{kind} {}", string_literal(v)),
        _ => string_literal(value),
    })
}

fn timestamp_text(v: &str) -> String {
    match v.parse::<f64>() {
        Ok(secs) => {
            let micros = (secs * 1_000_000.0).round() as i64;
            let dt = mongodb::bson::DateTime::from_millis(micros.div_euclid(1000));
            let base = dt.try_to_rfc3339_string().unwrap_or_else(|_| v.to_string());
            let frac = micros.rem_euclid(1_000_000);
            // RFC 3339 from bson has millisecond precision; show micros when present.
            match (frac % 1000 != 0, base.strip_suffix('Z')) {
                (true, Some(b)) => format!("{}{:06}Z", b.split('.').next().unwrap_or(b).to_string() + ".", frac),
                _ => base,
            }
        }
        Err(_) => v.to_string(),
    }
}

/// Converts one `{"v": …}` cell to JSON following the field schema.
fn cell_json(field: &Field, v: &Json) -> Json {
    if v.is_null() {
        return Json::Null;
    }
    if field.mode.as_deref() == Some("REPEATED") {
        let inner = Field { mode: None, ..field.clone() };
        return match v {
            Json::Array(items) => Json::Array(items.iter().map(|i| cell_json(&inner, &i["v"])).collect()),
            // The emulator sends repeated values as a JSON-encoded string.
            Json::String(s) => serde_json::from_str(s).unwrap_or_else(|_| v.clone()),
            other => other.clone(),
        };
    }
    match field.kind.as_str() {
        "RECORD" | "STRUCT" => {
            let values = v["f"].as_array().cloned().unwrap_or_default();
            Json::Object(
                field
                    .fields
                    .iter()
                    .zip(values.iter())
                    .map(|(f, c)| (f.name.clone(), cell_json(f, &c["v"])))
                    .collect(),
            )
        }
        "TIMESTAMP" => Json::String(timestamp_text(v.as_str().unwrap_or_default())),
        "INTEGER" | "INT64" | "FLOAT" | "FLOAT64" => v
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| v.clone()),
        "BOOLEAN" | "BOOL" => Json::Bool(v.as_str() == Some("true")),
        _ => v.clone(),
    }
}

fn cell_text(field: &Field, v: &Json) -> Cell {
    match cell_json(field, v) {
        Json::Null => None,
        Json::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

fn rows_to_cells(fields: &[Field], rows: &[Json]) -> Vec<Vec<Cell>> {
    rows.iter()
        .map(|r| {
            let cells = r["f"].as_array().cloned().unwrap_or_default();
            fields.iter().zip(cells.iter()).map(|(f, c)| cell_text(f, &c["v"])).collect()
        })
        .collect()
}

fn human_bytes(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[u]) }
}

impl BigQueryEngine {
    async fn access_token(&self) -> Result<Option<String>> {
        let creds = match &self.auth {
            Auth::Anonymous => return Ok(None),
            Auth::Credentials(c) => c,
        };
        let mut cached = self.token.lock().await;
        if let Some(t) = cached.as_ref().filter(|t| t.expires > Instant::now() + Duration::from_secs(60)) {
            return Ok(Some(t.value.clone()));
        }
        let token_uri = creds.token_uri.clone().unwrap_or_else(|| "https://oauth2.googleapis.com/token".into());
        let form: Vec<(&str, String)> = if creds.kind == "service_account" {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let claims = json!({
                "iss": creds.client_email.as_deref().context("key file has no client_email")?,
                "scope": SCOPE,
                "aud": token_uri,
                "iat": now,
                "exp": now + 3600,
            });
            let key = jsonwebtoken::EncodingKey::from_rsa_pem(
                creds.private_key.as_deref().context("key file has no private_key")?.as_bytes(),
            )?;
            let jwt = jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256), &claims, &key)?;
            vec![("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer".into()), ("assertion", jwt)]
        } else {
            vec![
                ("grant_type", "refresh_token".into()),
                ("client_id", creds.client_id.clone().unwrap_or_default()),
                ("client_secret", creds.client_secret.clone().unwrap_or_default()),
                ("refresh_token", creds.refresh_token.clone().unwrap_or_default()),
            ]
        };
        let resp = self.http.post(&token_uri).form(&form).send().await?;
        let status = resp.status();
        let body: Json = resp.json().await.unwrap_or(Json::Null);
        if !status.is_success() {
            bail!(
                "Google sign-in failed: {}",
                body["error_description"].as_str().or(body["error"].as_str()).unwrap_or(status.as_str())
            );
        }
        let value = body["access_token"].as_str().context("no access_token in the token response")?.to_string();
        let ttl = body["expires_in"].as_u64().unwrap_or(3600);
        *cached = Some(Token { value: value.clone(), expires: Instant::now() + Duration::from_secs(ttl) });
        Ok(Some(value))
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Json> {
        let req = match self.access_token().await? {
            Some(t) => req.bearer_auth(t),
            None => req,
        };
        let resp = req.send().await?;
        let status = resp.status();
        let body: Json = resp.json().await.unwrap_or(Json::Null);
        if !status.is_success() {
            let msg = body["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("HTTP {status}"));
            bail!("BigQuery: {msg}");
        }
        Ok(body)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/bigquery/v2{path}", self.endpoint)
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Json> {
        self.send(self.http.get(self.url(path)).query(query)).await
    }

    /// Follows `nextPageToken` across pages, collecting `items_key` arrays.
    async fn list_all(&self, path: &str, items_key: &str) -> Result<Vec<Json>> {
        let mut items = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = vec![("maxResults", "1000".to_string())];
            if let Some(t) = &token {
                q.push(("pageToken", t.clone()));
            }
            let q: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let body = self.get(path, &q).await?;
            items.extend(body[items_key].as_array().cloned().unwrap_or_default());
            match body["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(items)
    }

    async fn table(&self, dataset: &str, table: &str) -> Result<Json> {
        self.get(&format!("/projects/{}/datasets/{dataset}/tables/{table}", self.project), &[]).await
    }

    async fn fields(&self, dataset: &str, table: &str) -> Result<Vec<Field>> {
        let t = self.table(dataset, table).await?;
        Ok(serde_json::from_value(t["schema"]["fields"].clone()).unwrap_or_default())
    }

    /// Runs GoogleSQL and returns (schema, rows, response) once the job is done.
    async fn run_query(&self, sql: &str, max_rows: usize) -> Result<(Vec<Field>, Vec<Json>, Json)> {
        let body = json!({
            "query": sql,
            "useLegacySql": false,
            "maxResults": max_rows.min(10_000),
            "timeoutMs": 10_000,
        });
        let mut resp = self
            .send(self.http.post(self.url(&format!("/projects/{}/queries", self.project))).json(&body))
            .await?;
        let job_id = resp["jobReference"]["jobId"].as_str().unwrap_or_default().to_string();
        let location = resp["jobReference"]["location"].as_str().map(str::to_string);
        *self.running_job.lock().await = Some((job_id.clone(), location.clone()));
        let path = format!("/projects/{}/queries/{job_id}", self.project);
        let result: Result<_> = async {
            while resp["jobComplete"] != Json::Bool(true) {
                let mut q = vec![("timeoutMs", "10000".to_string()), ("maxResults", max_rows.min(10_000).to_string())];
                if let Some(l) = &location {
                    q.push(("location", l.clone()));
                }
                let q: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
                resp = self.get(&path, &q).await?;
            }
            let fields: Vec<Field> = serde_json::from_value(resp["schema"]["fields"].clone()).unwrap_or_default();
            let mut rows = resp["rows"].as_array().cloned().unwrap_or_default();
            let mut token = resp["pageToken"].as_str().map(str::to_string);
            while let Some(t) = token.filter(|_| rows.len() < max_rows) {
                let mut q = vec![("pageToken", t), ("maxResults", (max_rows - rows.len()).min(10_000).to_string())];
                if let Some(l) = &location {
                    q.push(("location", l.clone()));
                }
                let q: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let page = self.get(&path, &q).await?;
                rows.extend(page["rows"].as_array().cloned().unwrap_or_default());
                token = page["pageToken"].as_str().map(str::to_string);
            }
            Ok((fields, rows, resp.clone()))
        }
        .await;
        *self.running_job.lock().await = None;
        result
    }

    fn where_clause(&self, req: &FetchRequest, fields: &[Field]) -> Result<String> {
        let kind = |c: &str| fields.iter().find(|f| f.name == c).map(|f| f.kind.as_str()).unwrap_or("STRING");
        let mut parts = Vec::new();
        for f in &req.filters {
            let col = ident(&f.column);
            let text = format!("LOWER(CAST({col} AS STRING))");
            parts.push(match f.op.as_str() {
                "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                    let op = if f.op == "<>" { "!=" } else { f.op.as_str() };
                    format!("{col} {op} {}", typed_literal(&f.value, kind(&f.column))?)
                }
                "contains" => format!("{text} LIKE {}", string_literal(&format!("%{}%", f.value.to_lowercase()))),
                "notContains" => format!("NOT {text} LIKE {}", string_literal(&format!("%{}%", f.value.to_lowercase()))),
                "like" => format!("CAST({col} AS STRING) LIKE {}", string_literal(&f.value)),
                "in" => format!(
                    "{col} IN ({})",
                    f.value.split(',').map(|v| typed_literal(v, kind(&f.column))).collect::<Result<Vec<_>>>()?.join(", ")
                ),
                "isNull" => format!("{col} IS NULL"),
                "isNotNull" => format!("{col} IS NOT NULL"),
                other => bail!("unknown filter operator {other}"),
            });
        }
        if !req.raw_where.trim().is_empty() {
            parts.push(format!("({})", req.raw_where.trim()));
        }
        Ok(if parts.is_empty() { String::new() } else { format!(" WHERE {}", parts.join(" AND ")) })
    }

    fn table_ref(&self, req: &FetchRequest) -> String {
        ident(&format!("{}.{}.{}", self.project, req.schema, req.table))
    }
}

#[async_trait]
impl Driver for BigQueryEngine {
    async fn query(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let started = Instant::now();
        let (fields, rows, resp) = self.run_query(sql, MAX_RESULT_ROWS + 1).await?;
        let total = resp["totalRows"].as_str().and_then(|n| n.parse::<u64>().ok()).unwrap_or(rows.len() as u64);
        let mut cells = rows_to_cells(&fields, &rows);
        let truncated = cells.len() > MAX_RESULT_ROWS || total > cells.len() as u64;
        cells.truncate(MAX_RESULT_ROWS);
        let bytes = resp["totalBytesProcessed"].as_str().and_then(|n| n.parse::<u64>().ok());
        let mut result = QueryResult {
            sql: sql.to_string(),
            has_rows: !fields.is_empty(),
            columns: fields.iter().map(|f| ColumnMeta { name: f.name.clone(), data_type: Some(f.kind.clone()) }).collect(),
            rows: cells,
            truncated,
            rows_affected: resp["numDmlAffectedRows"].as_str().and_then(|n| n.parse().ok()).unwrap_or(0),
            duration_ms: started.elapsed().as_millis() as u64,
            ..Default::default()
        };
        if let Some(b) = bytes {
            let cached = resp["cacheHit"] == Json::Bool(true);
            result.notice = Some(if cached { "Served from cache (0 B billed)".into() } else { format!("Processed {}", human_bytes(b)) });
        }
        Ok(vec![result])
    }

    async fn cancel(&self) -> Result<()> {
        let Some((job, location)) = self.running_job.lock().await.clone() else { return Ok(()) };
        let mut q = Vec::new();
        if let Some(l) = &location {
            q.push(("location", l.as_str()));
        }
        self.send(self.http.post(self.url(&format!("/projects/{}/jobs/{job}/cancel", self.project))).query(&q))
            .await?;
        Ok(())
    }

    async fn close(&self) {}
}

#[async_trait]
impl Browser for BigQueryEngine {
    async fn list_schemas(&self) -> Result<Vec<String>> {
        let items = self.list_all(&format!("/projects/{}/datasets", self.project), "datasets").await?;
        let mut names: Vec<String> = items
            .iter()
            .filter_map(|d| d["datasetReference"]["datasetId"].as_str().map(str::to_string))
            .collect();
        names.sort();
        Ok(names)
    }

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>> {
        if schema.is_empty() {
            return Ok(Vec::new());
        }
        let items = self.list_all(&format!("/projects/{}/datasets/{schema}/tables", self.project), "tables").await?;
        let mut out: Vec<TableInfo> = items
            .iter()
            .filter_map(|t| {
                let name = t["tableReference"]["tableId"].as_str()?.to_string();
                let kind = match t["type"].as_str().unwrap_or("TABLE") {
                    "VIEW" => "view",
                    "MATERIALIZED_VIEW" => "materialized_view",
                    "EXTERNAL" => "foreign_table",
                    _ => "table",
                };
                Some(TableInfo { name, kind: kind.into() })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn table_columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        Ok(self
            .fields(schema, table)
            .await?
            .into_iter()
            .map(|f| ColumnInfo {
                nullable: f.mode.as_deref() != Some("REQUIRED"),
                data_type: if f.mode.as_deref() == Some("REPEATED") { format!("ARRAY<{}>", f.kind) } else { f.kind.clone() },
                name: f.name,
                default: None,
                is_primary_key: false,
            })
            .collect())
    }

    async fn schema_columns(&self, schema: &str) -> Result<BTreeMap<String, Vec<String>>> {
        let tables = self.list_tables(schema).await?;
        let lookups = tables.iter().take(50).map(|t| async move {
            let cols = self.fields(schema, &t.name).await.unwrap_or_default();
            (t.name.clone(), cols.into_iter().map(|f| f.name).collect::<Vec<_>>())
        });
        Ok(futures_util::future::join_all(lookups).await.into_iter().collect())
    }

    async fn fetch_rows(&self, req: &FetchRequest) -> Result<QueryResult> {
        let started = Instant::now();
        let meta = self.table(&req.schema, &req.table).await?;
        let fields: Vec<Field> = serde_json::from_value(meta["schema"]["fields"].clone()).unwrap_or_default();
        let plain_table = meta["type"].as_str().unwrap_or("TABLE") == "TABLE";
        let columns: Vec<ColumnMeta> =
            fields.iter().map(|f| ColumnMeta { name: f.name.clone(), data_type: Some(f.kind.clone()) }).collect();

        if plain_table && req.filters.is_empty() && req.raw_where.trim().is_empty() && req.sort.is_none() {
            let path = format!("/projects/{}/datasets/{}/tables/{}/data", self.project, req.schema, req.table);
            let start = req.offset.to_string();
            let max = req.limit.to_string();
            let body = self.get(&path, &[("startIndex", start.as_str()), ("maxResults", max.as_str())]).await?;
            let rows = body["rows"].as_array().cloned().unwrap_or_default();
            return Ok(QueryResult {
                sql: format!("tabledata.list {}.{} (free)", req.schema, req.table),
                columns,
                rows: rows_to_cells(&fields, &rows),
                has_rows: true,
                duration_ms: started.elapsed().as_millis() as u64,
                notice: Some("Browsing with tabledata.list — no query cost".into()),
                ..Default::default()
            });
        }

        let mut sql = format!("SELECT * FROM {}{}", self.table_ref(req), self.where_clause(req, &fields)?);
        if let Some(s) = &req.sort {
            sql.push_str(&format!(" ORDER BY {} {}", ident(&s.column), if s.desc { "DESC" } else { "ASC" }));
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));
        let mut result = self.query(&sql).await?.remove(0);
        result.duration_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }

    async fn count_rows(&self, req: &FetchRequest) -> Result<u64> {
        let meta = self.table(&req.schema, &req.table).await?;
        let plain_table = meta["type"].as_str().unwrap_or("TABLE") == "TABLE";
        if plain_table && req.filters.is_empty() && req.raw_where.trim().is_empty() {
            if let Some(n) = meta["numRows"].as_str().and_then(|n| n.parse().ok()) {
                return Ok(n);
            }
        }
        let fields: Vec<Field> = serde_json::from_value(meta["schema"]["fields"].clone()).unwrap_or_default();
        let sql = format!("SELECT COUNT(*) FROM {}{}", self.table_ref(req), self.where_clause(req, &fields)?);
        let (_, rows, _) = self.run_query(&sql, 1).await?;
        rows.first()
            .and_then(|r| r["f"][0]["v"].as_str())
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| anyhow!("unexpected COUNT(*) result"))
    }

    async fn preview_changes(&self, _set: &ChangeSet) -> Result<Vec<String>> {
        bail!("BigQuery tables are read-only here; use DML in the query editor")
    }

    async fn commit_changes(&self, _set: &ChangeSet) -> Result<u64> {
        bail!("BigQuery tables are read-only here; use DML in the query editor")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_follow_column_types() {
        assert_eq!(typed_literal("42", "INTEGER").unwrap(), "42");
        assert!(typed_literal("4x", "INT64").is_err());
        assert_eq!(typed_literal("true", "BOOLEAN").unwrap(), "TRUE");
        assert_eq!(typed_literal("2026-01-02", "DATE").unwrap(), "DATE '2026-01-02'");
        assert_eq!(typed_literal("it's \\", "STRING").unwrap(), r"'it\'s \\'");
        assert_eq!(ident("p.d.t`x"), r"`p.d.t\`x`");
    }

    #[test]
    fn converts_nested_rows() {
        let fields: Vec<Field> = serde_json::from_value(json!([
            {"name": "id", "type": "INTEGER", "mode": "REQUIRED"},
            {"name": "tags", "type": "STRING", "mode": "REPEATED"},
            {"name": "addr", "type": "RECORD", "fields": [{"name": "city", "type": "STRING"}, {"name": "zip", "type": "INTEGER"}]},
            {"name": "at", "type": "TIMESTAMP"},
            {"name": "note", "type": "STRING"}
        ]))
        .unwrap();
        let rows = vec![json!({"f": [
            {"v": "7"},
            {"v": [{"v": "a"}, {"v": "b"}]},
            {"v": {"f": [{"v": "Oslo"}, {"v": "150"}]}},
            {"v": "1.7e9"},
            {"v": null}
        ]})];
        assert_eq!(
            rows_to_cells(&fields, &rows)[0],
            vec![
                Some("7".into()),
                Some(r#"["a","b"]"#.into()),
                Some(r#"{"city":"Oslo","zip":150}"#.into()),
                Some("2023-11-14T22:13:20Z".into()),
                None
            ]
        );
    }
}
