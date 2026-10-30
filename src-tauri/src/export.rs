//! Exports: a whole table (or a filtered, sorted view of it) written to a file
//! a page at a time so large tables never sit in memory, plus a plain text
//! writer for results the UI already holds.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::oneshot;

use crate::dialect::{Dialect, SqlEngine};
use crate::drivers::bigquery;
use crate::model::*;
use crate::session::Session;
use crate::{err, AppState, CmdResult};

/// Rows fetched per round trip; well under the drivers' result cap.
const PAGE_SIZE: u64 = 5_000;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Json,
    /// One INSERT statement per row.
    Sql,
}

/// A `FetchRequest` without paging: which rows to export, in what order.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub schema: String,
    pub table: String,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub raw_where: String,
    #[serde(default)]
    pub sort: Option<Sort>,
    #[serde(default)]
    pub key_order: Vec<String>,
}

impl ExportRequest {
    fn page(&self, offset: u64) -> FetchRequest {
        FetchRequest {
            schema: self.schema.clone(),
            table: self.table.clone(),
            limit: PAGE_SIZE,
            offset,
            filters: self.filters.clone(),
            raw_where: self.raw_where.clone(),
            sort: self.sort.clone(),
            key_order: self.key_order.clone(),
        }
    }
}

/// Writes the rows `request` selects to `path` and returns how many there were.
#[tauri::command]
pub async fn export_table(
    state: State<'_, AppState>,
    session_id: String,
    request: ExportRequest,
    format: ExportFormat,
    path: String,
) -> CmdResult<u64> {
    let session = state.session(&session_id).await?;
    match write_table(&session, &request, format, &path).await {
        Ok(n) => Ok(n),
        Err(e) => {
            // Don't leave half a file behind.
            let _ = tokio::fs::remove_file(&path).await;
            Err(err(e))
        }
    }
}

#[tauri::command]
pub async fn write_text_file(path: String, contents: String) -> CmdResult<()> {
    tokio::fs::write(&path, contents).await.map_err(|e| format!("Couldn't write {path}: {e}"))
}

#[derive(Serialize)]
pub struct PickedFile {
    name: String,
    contents: String,
}

/// Asks the user for a file and returns its text. The path is chosen here,
/// never by the UI, so the renderer can only read files the user picked.
#[tauri::command]
pub async fn pick_text_file<R: Runtime>(app: AppHandle<R>, extensions: Vec<String>) -> CmdResult<Option<PickedFile>> {
    let (tx, rx) = oneshot::channel();
    let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
    app.dialog().file().add_filter("Data file", &extensions).pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    let Some(picked) = rx.await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    let contents = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Some(PickedFile { name, contents }))
}

pub(crate) async fn write_table(session: &Session, req: &ExportRequest, format: ExportFormat, path: &str) -> Result<u64> {
    let driver = session.info().await.driver;
    let quoting = Quoting::for_driver(driver);
    let mut enc = Encoder::new(format, quoting, quoting.table(&req.schema, &req.table));
    let file = File::create(path).await.with_context(|| format!("couldn't create {path}"))?;
    let mut out = BufWriter::new(file);
    let mut offset = 0;
    loop {
        let page = session.fetch_rows(&req.page(offset)).await?;
        let columns: Vec<String> = page.columns.into_iter().map(|c| c.name).collect();
        if offset == 0 {
            out.write_all(enc.start(&columns).as_bytes()).await?;
        }
        out.write_all(enc.page(&columns, &page.rows).as_bytes()).await?;
        let n = page.rows.len() as u64;
        offset += n;
        // BigQuery's free table listing may return short pages before the end.
        if n == 0 || (n < PAGE_SIZE && driver != DriverKind::Bigquery) {
            break;
        }
    }
    out.write_all(enc.finish().as_bytes()).await?;
    out.flush().await?;
    Ok(offset)
}

/// Identifier and literal quoting for INSERT statements, per engine. Engines
/// without SQL get standard double-quoted identifiers.
#[derive(Clone, Copy)]
enum Quoting {
    Sql(Dialect),
    BigQuery,
}

impl Quoting {
    fn for_driver(kind: DriverKind) -> Self {
        match kind {
            DriverKind::Bigquery => Quoting::BigQuery,
            _ => Quoting::Sql(Dialect::for_driver(kind).unwrap_or(Dialect(SqlEngine::Postgres))),
        }
    }

    fn ident(&self, name: &str) -> String {
        match self {
            Quoting::Sql(d) => d.ident(name),
            Quoting::BigQuery => bigquery::ident(name),
        }
    }

    fn table(&self, schema: &str, table: &str) -> String {
        match self {
            Quoting::Sql(d) => d.table(schema, table),
            Quoting::BigQuery if schema.is_empty() => bigquery::ident(table),
            Quoting::BigQuery => format!("{}.{}", bigquery::ident(schema), bigquery::ident(table)),
        }
    }

    fn value(&self, value: &Cell) -> String {
        match (self, value) {
            (_, None) => "NULL".into(),
            (Quoting::Sql(d), Some(v)) => d.literal(v),
            (Quoting::BigQuery, Some(v)) => bigquery::string_literal(v),
        }
    }
}

/// Turns pages of rows into file text. CSV columns follow the first page;
/// JSON and SQL use each page's own columns (MongoDB pages can differ).
struct Encoder {
    format: ExportFormat,
    quoting: Quoting,
    table: String,
    header: Vec<String>,
    rows: u64,
}

impl Encoder {
    fn new(format: ExportFormat, quoting: Quoting, table: String) -> Self {
        Self { format, quoting, table, header: Vec::new(), rows: 0 }
    }

    fn start(&mut self, columns: &[String]) -> String {
        self.header = columns.to_vec();
        match self.format {
            ExportFormat::Csv => csv_line(columns.iter().map(|c| Some(c.as_str()))),
            ExportFormat::Json => "[".into(),
            ExportFormat::Sql => String::new(),
        }
    }

    fn page(&mut self, columns: &[String], rows: &[Vec<Cell>]) -> String {
        let mut out = String::new();
        match self.format {
            ExportFormat::Csv => {
                let at: Vec<Option<usize>> = self.header.iter().map(|h| columns.iter().position(|c| c == h)).collect();
                for row in rows {
                    out.push_str(&csv_line(at.iter().map(|i| i.and_then(|i| row.get(i)?.as_deref()))));
                }
            }
            ExportFormat::Json => {
                let keys: Vec<String> = columns.iter().map(|c| json_string(c)).collect();
                for (n, row) in rows.iter().enumerate() {
                    out.push_str(if self.rows + n as u64 == 0 { "\n  {" } else { ",\n  {" });
                    for (i, key) in keys.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(key);
                        out.push_str(": ");
                        match row.get(i).and_then(Option::as_deref) {
                            Some(v) => out.push_str(&json_string(v)),
                            None => out.push_str("null"),
                        }
                    }
                    out.push('}');
                }
            }
            ExportFormat::Sql => {
                let cols = columns.iter().map(|c| self.quoting.ident(c)).collect::<Vec<_>>().join(", ");
                for row in rows {
                    let values = (0..columns.len())
                        .map(|i| self.quoting.value(row.get(i).unwrap_or(&None)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!("INSERT INTO {} ({cols}) VALUES ({values});\n", self.table));
                }
            }
        }
        self.rows += rows.len() as u64;
        out
    }

    fn finish(&self) -> String {
        match self.format {
            ExportFormat::Json if self.rows == 0 => "]\n".into(),
            ExportFormat::Json => "\n]\n".into(),
            _ => String::new(),
        }
    }
}

/// One CSV line; fields are quoted only when needed and NULL is empty.
fn csv_line<'a>(fields: impl Iterator<Item = Option<&'a str>>) -> String {
    let mut line = fields
        .map(|f| match f {
            None => String::new(),
            Some(v) if v.contains([',', '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
            Some(v) => v.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    line.push('\n');
    line
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("strings always serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn csv_quotes_only_when_needed_and_follows_the_first_header() {
        let mut enc = Encoder::new(ExportFormat::Csv, Quoting::for_driver(DriverKind::Postgres), String::new());
        let mut text = enc.start(&cols(&["id", "note"]));
        text += &enc.page(&cols(&["id", "note"]), &[vec![Some("1".into()), Some("a, \"b\"".into())]]);
        text += &enc.page(&cols(&["note", "id"]), &[vec![None, Some("2".into())]]);
        assert_eq!(text, "id,note\n1,\"a, \"\"b\"\"\"\n2,\n");
    }

    #[test]
    fn json_is_an_array_of_objects_in_column_order() {
        let mut enc = Encoder::new(ExportFormat::Json, Quoting::for_driver(DriverKind::Sqlite), String::new());
        let mut text = enc.start(&cols(&["b", "a"]));
        text += &enc.page(&cols(&["b", "a"]), &[vec![Some("x\"".into()), None], vec![None, Some("1".into())]]);
        text += &enc.finish();
        assert_eq!(text, "[\n  {\"b\": \"x\\\"\", \"a\": null},\n  {\"b\": null, \"a\": \"1\"}\n]\n");
    }

    #[test]
    fn inserts_quote_per_engine() {
        let q = Quoting::for_driver(DriverKind::Mysql);
        let mut enc = Encoder::new(ExportFormat::Sql, q, q.table("shop", "it's"));
        let text = enc.page(&cols(&["na`me"]), &[vec![Some("O'Brien\\".into())], vec![None]]);
        assert_eq!(
            text,
            "INSERT INTO `shop`.`it's` (`na``me`) VALUES ('O''Brien\\\\');\nINSERT INTO `shop`.`it's` (`na``me`) VALUES (NULL);\n"
        );
    }
}
