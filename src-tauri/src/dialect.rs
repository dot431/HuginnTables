//! SQL generation for each supported engine: identifier quoting, literals,
//! catalog queries, table paging and staged-change statements.

use anyhow::{bail, Result};

use crate::model::{CellValue, ChangeSet, DriverKind, FetchRequest, Filter, RowChange};

#[derive(Clone, Copy, Debug)]
pub struct Dialect(pub DriverKind);

impl Dialect {
    pub fn ident(&self, name: &str) -> String {
        match self.0 {
            DriverKind::Mysql => format!("`{}`", name.replace('`', "``")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    pub fn table(&self, schema: &str, table: &str) -> String {
        if schema.is_empty() {
            self.ident(table)
        } else {
            format!("{}.{}", self.ident(schema), self.ident(table))
        }
    }

    pub fn literal(&self, value: &str) -> String {
        match self.0 {
            DriverKind::Mysql => {
                let mut out = String::with_capacity(value.len() + 2);
                out.push('\'');
                for ch in value.chars() {
                    match ch {
                        '\'' => out.push_str("''"),
                        '\\' => out.push_str("\\\\"),
                        '\0' => out.push_str("\\0"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\x1a' => out.push_str("\\Z"),
                        c => out.push(c),
                    }
                }
                out.push('\'');
                out
            }
            _ => format!("'{}'", value.replace('\'', "''")),
        }
    }

    fn value(&self, value: &Option<String>) -> String {
        match value {
            Some(v) => self.literal(v),
            None => "NULL".into(),
        }
    }

    // ---- catalog -------------------------------------------------------

    pub fn version_sql(&self) -> &'static str {
        match self.0 {
            DriverKind::Postgres => "SHOW server_version",
            DriverKind::Mysql => "SELECT VERSION()",
            DriverKind::Sqlite => "SELECT 'SQLite ' || sqlite_version()",
        }
    }

    pub fn current_schema_sql(&self) -> &'static str {
        match self.0 {
            DriverKind::Postgres => "SELECT current_schema()",
            DriverKind::Mysql => "SELECT DATABASE()",
            DriverKind::Sqlite => "SELECT 'main'",
        }
    }

    pub fn databases_sql(&self) -> Option<&'static str> {
        match self.0 {
            DriverKind::Postgres => Some(
                "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY 1",
            ),
            _ => None,
        }
    }

    pub fn schemas_sql(&self) -> &'static str {
        match self.0 {
            DriverKind::Postgres => {
                "SELECT nspname FROM pg_namespace \
                 WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' \
                 ORDER BY nspname = 'public' DESC, nspname"
            }
            DriverKind::Mysql => "SHOW DATABASES",
            DriverKind::Sqlite => "SELECT name FROM pragma_database_list ORDER BY seq",
        }
    }

    /// Rows of (name, kind).
    pub fn tables_sql(&self, schema: &str) -> String {
        match self.0 {
            DriverKind::Postgres => format!(
                "SELECT c.relname, CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view' \
                 WHEN 'f' THEN 'foreign_table' ELSE 'table' END \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relkind IN ('r','p','v','m','f') \
                 AND NOT c.relispartition ORDER BY c.relname",
                self.literal(schema)
            ),
            DriverKind::Mysql => format!(
                "SELECT TABLE_NAME, CASE TABLE_TYPE WHEN 'VIEW' THEN 'view' ELSE 'table' END \
                 FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
                self.literal(schema)
            ),
            DriverKind::Sqlite => format!(
                "SELECT name, type FROM {}.sqlite_master \
                 WHERE type IN ('table','view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
                self.ident(schema)
            ),
        }
    }

    /// Rows of (name, type, nullable 'YES'/'NO', default, is_pk 'YES'/'NO').
    pub fn columns_sql(&self, schema: &str, table: &str) -> String {
        match self.0 {
            DriverKind::Postgres => format!(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), \
                 CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, pg_get_expr(d.adbin, d.adrelid), \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid \
                   AND i.indisprimary AND a.attnum = ANY(i.indkey)) THEN 'YES' ELSE 'NO' END \
                 FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                self.literal(&self.table(schema, table))
            ),
            DriverKind::Mysql => format!(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, \
                 CASE WHEN COLUMN_KEY = 'PRI' THEN 'YES' ELSE 'NO' END \
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
                 ORDER BY ORDINAL_POSITION",
                self.literal(schema),
                self.literal(table)
            ),
            DriverKind::Sqlite => format!(
                "SELECT name, type, CASE WHEN \"notnull\" THEN 'NO' ELSE 'YES' END, dflt_value, \
                 CASE WHEN pk > 0 THEN 'YES' ELSE 'NO' END \
                 FROM pragma_table_info({}, {}) ORDER BY cid",
                self.literal(table),
                self.literal(schema)
            ),
        }
    }

    /// Rows of (table, column) for every table in a schema, for autocomplete.
    pub fn schema_columns_sql(&self, schema: &str) -> String {
        match self.0 {
            DriverKind::Postgres => format!(
                "SELECT c.relname, a.attname FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relkind IN ('r','p','v','m','f') AND a.attnum > 0 \
                 AND NOT a.attisdropped ORDER BY c.relname, a.attnum",
                self.literal(schema)
            ),
            DriverKind::Mysql => format!(
                "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME, ORDINAL_POSITION",
                self.literal(schema)
            ),
            DriverKind::Sqlite => format!(
                "SELECT m.name, p.name FROM {}.sqlite_master m, pragma_table_info(m.name, {}) p \
                 WHERE m.type IN ('table','view') ORDER BY m.name, p.cid",
                self.ident(schema),
                self.literal(schema)
            ),
        }
    }

    // ---- browsing ------------------------------------------------------

    fn text_expr(&self, column: &str) -> String {
        match self.0 {
            DriverKind::Postgres => format!("{}::text", self.ident(column)),
            _ => self.ident(column),
        }
    }

    fn filter_sql(&self, f: &Filter) -> Result<String> {
        let col = self.ident(&f.column);
        let like = match self.0 {
            DriverKind::Postgres => "ILIKE",
            _ => "LIKE",
        };
        let v = self.literal(&f.value);
        Ok(match f.op.as_str() {
            "=" | "<>" | "<" | ">" | "<=" | ">=" => format!("{col} {} {v}", f.op),
            "contains" => format!(
                "{} {like} {}",
                self.text_expr(&f.column),
                self.literal(&format!("%{}%", f.value))
            ),
            "notContains" => format!(
                "{} NOT {like} {}",
                self.text_expr(&f.column),
                self.literal(&format!("%{}%", f.value))
            ),
            "like" => format!("{} {like} {v}", self.text_expr(&f.column)),
            "in" => {
                let items: Vec<String> = f
                    .value
                    .split(',')
                    .map(|s| self.literal(s.trim()))
                    .collect();
                format!("{col} IN ({})", items.join(", "))
            }
            "isNull" => format!("{col} IS NULL"),
            "isNotNull" => format!("{col} IS NOT NULL"),
            other => bail!("unknown filter operator {other}"),
        })
    }

    fn where_clause(&self, req: &FetchRequest) -> Result<String> {
        let mut parts = Vec::new();
        for f in &req.filters {
            parts.push(self.filter_sql(f)?);
        }
        if !req.raw_where.trim().is_empty() {
            parts.push(format!("({})", req.raw_where.trim()));
        }
        Ok(if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(" AND "))
        })
    }

    pub fn select_page_sql(&self, req: &FetchRequest) -> Result<String> {
        let mut sql = format!(
            "SELECT * FROM {}{}",
            self.table(&req.schema, &req.table),
            self.where_clause(req)?
        );
        if let Some(sort) = &req.sort {
            sql.push_str(&format!(
                " ORDER BY {} {}",
                self.ident(&sort.column),
                if sort.desc { "DESC" } else { "ASC" }
            ));
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));
        Ok(sql)
    }

    pub fn count_sql(&self, req: &FetchRequest) -> Result<String> {
        Ok(format!(
            "SELECT COUNT(*) FROM {}{}",
            self.table(&req.schema, &req.table),
            self.where_clause(req)?
        ))
    }

    // ---- staged changes ------------------------------------------------

    fn match_clause(&self, key: &[CellValue], primary: bool) -> String {
        key.iter()
            .map(|c| {
                // Without a primary key every column takes part in matching;
                // compare as text in Postgres so types without `=` (json, xml…) work.
                let col = if primary {
                    self.ident(&c.column)
                } else {
                    self.text_expr(&c.column)
                };
                match &c.value {
                    None => format!("{col} IS NULL"),
                    Some(v) => format!("{col} = {}", self.literal(v)),
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// Restricts a non-primary-key match to a single physical row.
    fn single_row_where(&self, table: &str, cond: &str, primary: bool) -> (String, &'static str) {
        if primary {
            return (cond.to_string(), "");
        }
        match self.0 {
            DriverKind::Postgres => (
                format!("ctid = (SELECT ctid FROM {table} WHERE {cond} LIMIT 1)"),
                "",
            ),
            DriverKind::Sqlite => (
                format!("rowid = (SELECT rowid FROM {table} WHERE {cond} LIMIT 1)"),
                "",
            ),
            DriverKind::Mysql => (cond.to_string(), " LIMIT 1"),
        }
    }

    pub fn change_statements(&self, set: &ChangeSet) -> Result<Vec<String>> {
        let table = self.table(&set.schema, &set.table);
        let mut out = Vec::with_capacity(set.changes.len());
        for change in &set.changes {
            match change {
                RowChange::Insert { values } => {
                    if values.is_empty() {
                        out.push(match self.0 {
                            DriverKind::Mysql => format!("INSERT INTO {table} () VALUES ()"),
                            _ => format!("INSERT INTO {table} DEFAULT VALUES"),
                        });
                    } else {
                        let cols: Vec<_> = values.iter().map(|c| self.ident(&c.column)).collect();
                        let vals: Vec<_> = values.iter().map(|c| self.value(&c.value)).collect();
                        out.push(format!(
                            "INSERT INTO {table} ({}) VALUES ({})",
                            cols.join(", "),
                            vals.join(", ")
                        ));
                    }
                }
                RowChange::Update { key, values } => {
                    if values.is_empty() {
                        continue;
                    }
                    if key.is_empty() {
                        bail!("cannot update a row of {table} without identifying columns");
                    }
                    let assignments: Vec<_> = values
                        .iter()
                        .map(|c| format!("{} = {}", self.ident(&c.column), self.value(&c.value)))
                        .collect();
                    let cond = self.match_clause(key, set.key_is_primary);
                    let (cond, limit) = self.single_row_where(&table, &cond, set.key_is_primary);
                    out.push(format!(
                        "UPDATE {table} SET {} WHERE {cond}{limit}",
                        assignments.join(", ")
                    ));
                }
                RowChange::Delete { key } => {
                    if key.is_empty() {
                        bail!("cannot delete a row of {table} without identifying columns");
                    }
                    let cond = self.match_clause(key, set.key_is_primary);
                    let (cond, limit) = self.single_row_where(&table, &cond, set.key_is_primary);
                    out.push(format!("DELETE FROM {table} WHERE {cond}{limit}"));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Sort;

    fn cv(c: &str, v: Option<&str>) -> CellValue {
        CellValue { column: c.into(), value: v.map(Into::into) }
    }

    #[test]
    fn quoting() {
        let pg = Dialect(DriverKind::Postgres);
        let my = Dialect(DriverKind::Mysql);
        assert_eq!(pg.ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(my.ident("we`ird"), "`we``ird`");
        assert_eq!(pg.literal("it's \\"), "'it''s \\'");
        assert_eq!(my.literal("it's \\\n"), "'it''s \\\\\\n'");
    }

    #[test]
    fn page_query() {
        let pg = Dialect(DriverKind::Postgres);
        let req = FetchRequest {
            schema: "public".into(),
            table: "users".into(),
            limit: 100,
            offset: 200,
            filters: vec![
                Filter { column: "name".into(), op: "contains".into(), value: "ann".into() },
                Filter { column: "deleted_at".into(), op: "isNull".into(), value: String::new() },
            ],
            raw_where: String::new(),
            sort: Some(Sort { column: "id".into(), desc: true }),
        };
        assert_eq!(
            pg.select_page_sql(&req).unwrap(),
            "SELECT * FROM \"public\".\"users\" WHERE \"name\"::text ILIKE '%ann%' AND \"deleted_at\" IS NULL ORDER BY \"id\" DESC LIMIT 100 OFFSET 200"
        );
    }

    #[test]
    fn changes() {
        let my = Dialect(DriverKind::Mysql);
        let set = ChangeSet {
            schema: "shop".into(),
            table: "items".into(),
            key_is_primary: true,
            changes: vec![
                RowChange::Update { key: vec![cv("id", Some("7"))], values: vec![cv("name", Some("O'Brien")), cv("note", None)] },
                RowChange::Insert { values: vec![cv("name", Some("x"))] },
                RowChange::Delete { key: vec![cv("id", Some("9"))] },
            ],
        };
        assert_eq!(
            my.change_statements(&set).unwrap(),
            vec![
                "UPDATE `shop`.`items` SET `name` = 'O''Brien', `note` = NULL WHERE `id` = '7'",
                "INSERT INTO `shop`.`items` (`name`) VALUES ('x')",
                "DELETE FROM `shop`.`items` WHERE `id` = '9'",
            ]
        );

        let pg = Dialect(DriverKind::Postgres);
        let set = ChangeSet {
            schema: "public".into(),
            table: "logs".into(),
            key_is_primary: false,
            changes: vec![RowChange::Delete { key: vec![cv("msg", Some("a")), cv("at", None)] }],
        };
        assert_eq!(
            pg.change_statements(&set).unwrap(),
            vec!["DELETE FROM \"public\".\"logs\" WHERE ctid = (SELECT ctid FROM \"public\".\"logs\" WHERE \"msg\"::text = 'a' AND \"at\"::text IS NULL LIMIT 1)"]
        );
    }
}
