//! SQL generation for each supported engine: identifier quoting, literals,
//! catalog queries, table paging and staged-change statements.

use anyhow::{bail, Result};

use crate::model::{Cell, CellValue, ChangeSet, DriverKind, FetchRequest, Filter, IndexInfo, RowChange, SchemaChange};

/// `table(columns)` references `parent_table(parent_columns)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignKey {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub columns: Vec<String>,
    pub parent_schema: String,
    pub parent_table: String,
    pub parent_columns: Vec<String>,
    pub on_delete: String,
}

impl ForeignKey {
    /// Groups the rows of `Dialect::foreign_keys_sql` (one per column) into keys.
    pub fn from_rows(rows: Vec<Vec<Cell>>) -> Vec<ForeignKey> {
        let mut out: Vec<ForeignKey> = Vec::new();
        for row in rows {
            let mut it = row.into_iter().map(Option::unwrap_or_default);
            let mut next = || it.next().unwrap_or_default();
            let (schema, table, name, column) = (next(), next(), next(), next());
            let (parent_schema, parent_table, parent_column, on_delete) = (next(), next(), next(), next());
            match out.last_mut() {
                Some(fk) if fk.schema == schema && fk.table == table && fk.name == name => {
                    fk.columns.push(column);
                    fk.parent_columns.push(parent_column);
                }
                _ => out.push(ForeignKey {
                    schema,
                    table,
                    name,
                    columns: vec![column],
                    parent_schema,
                    parent_table,
                    parent_columns: vec![parent_column],
                    on_delete: on_delete.to_uppercase(),
                }),
            }
        }
        out
    }

    /// ON DELETE SET NULL / SET DEFAULT: the database keeps the referencing
    /// rows and clears the reference itself, so they never block a delete.
    pub fn sets_null(&self) -> bool {
        self.on_delete.starts_with("SET ")
    }
}

/// Rows of `fk.table` that must be deleted before the staged deletes can run:
/// those `filter` selects, i.e. whose `fk.columns` point at doomed rows.
#[derive(Clone, Debug)]
pub struct CascadeStep {
    pub fk: ForeignKey,
    /// 1 for direct references to the deleted rows.
    pub depth: u32,
    pub filter: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlEngine {
    Postgres,
    Mysql,
    Sqlite,
}

#[derive(Clone, Copy, Debug)]
pub struct Dialect(pub SqlEngine);

impl Dialect {
    pub fn for_driver(kind: DriverKind) -> Option<Self> {
        match kind {
            DriverKind::Postgres => Some(Dialect(SqlEngine::Postgres)),
            DriverKind::Mysql => Some(Dialect(SqlEngine::Mysql)),
            DriverKind::Sqlite => Some(Dialect(SqlEngine::Sqlite)),
            _ => None,
        }
    }

    pub fn ident(&self, name: &str) -> String {
        match self.0 {
            SqlEngine::Mysql => format!("`{}`", name.replace('`', "``")),
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
            SqlEngine::Mysql => {
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
            SqlEngine::Postgres => "SHOW server_version",
            SqlEngine::Mysql => "SELECT VERSION()",
            SqlEngine::Sqlite => "SELECT 'SQLite ' || sqlite_version()",
        }
    }

    pub fn current_schema_sql(&self) -> &'static str {
        match self.0 {
            SqlEngine::Postgres => "SELECT current_schema()",
            SqlEngine::Mysql => "SELECT DATABASE()",
            SqlEngine::Sqlite => "SELECT 'main'",
        }
    }

    pub fn databases_sql(&self) -> Option<&'static str> {
        match self.0 {
            SqlEngine::Postgres => Some(
                "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY 1",
            ),
            _ => None,
        }
    }

    pub fn schemas_sql(&self) -> &'static str {
        match self.0 {
            SqlEngine::Postgres => {
                "SELECT nspname FROM pg_namespace \
                 WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' \
                 ORDER BY nspname = 'public' DESC, nspname"
            }
            SqlEngine::Mysql => "SHOW DATABASES",
            SqlEngine::Sqlite => "SELECT name FROM pragma_database_list ORDER BY seq",
        }
    }

    /// Rows of (name, kind).
    pub fn tables_sql(&self, schema: &str) -> String {
        match self.0 {
            SqlEngine::Postgres => format!(
                "SELECT c.relname, CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view' \
                 WHEN 'f' THEN 'foreign_table' ELSE 'table' END \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relkind IN ('r','p','v','m','f') \
                 AND NOT c.relispartition ORDER BY c.relname",
                self.literal(schema)
            ),
            SqlEngine::Mysql => format!(
                "SELECT TABLE_NAME, CASE TABLE_TYPE WHEN 'VIEW' THEN 'view' ELSE 'table' END \
                 FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
                self.literal(schema)
            ),
            SqlEngine::Sqlite => format!(
                "SELECT name, type FROM {}.sqlite_master \
                 WHERE type IN ('table','view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
                self.ident(schema)
            ),
        }
    }

    /// Rows of (name, type, nullable 'YES'/'NO', default, is_pk 'YES'/'NO').
    pub fn columns_sql(&self, schema: &str, table: &str) -> String {
        match self.0 {
            SqlEngine::Postgres => format!(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), \
                 CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, pg_get_expr(d.adbin, d.adrelid), \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid \
                   AND i.indisprimary AND a.attnum = ANY(i.indkey)) THEN 'YES' ELSE 'NO' END \
                 FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                self.literal(&self.table(schema, table))
            ),
            SqlEngine::Mysql => format!(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, \
                 CASE WHEN COLUMN_KEY = 'PRI' THEN 'YES' ELSE 'NO' END \
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
                 ORDER BY ORDINAL_POSITION",
                self.literal(schema),
                self.literal(table)
            ),
            SqlEngine::Sqlite => format!(
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
            SqlEngine::Postgres => format!(
                "SELECT c.relname, a.attname FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relkind IN ('r','p','v','m','f') AND a.attnum > 0 \
                 AND NOT a.attisdropped ORDER BY c.relname, a.attnum",
                self.literal(schema)
            ),
            SqlEngine::Mysql => format!(
                "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME, ORDINAL_POSITION",
                self.literal(schema)
            ),
            SqlEngine::Sqlite => format!(
                "SELECT m.name, p.name FROM {}.sqlite_master m, pragma_table_info(m.name, {}) p \
                 WHERE m.type IN ('table','view') ORDER BY m.name, p.cid",
                self.ident(schema),
                self.literal(schema)
            ),
        }
    }

    /// Every foreign key that can reach `schema`, one row per column in key
    /// order: (schema, table, constraint, column, parent schema, parent table,
    /// parent column, ON DELETE rule). SQLite keys never leave their database.
    pub fn foreign_keys_sql(&self, schema: &str) -> String {
        match self.0 {
            SqlEngine::Postgres => "SELECT cn.nspname, cl.relname, c.conname, a.attname, pn.nspname, pl.relname, pa.attname, \
                 CASE c.confdeltype WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' \
                 WHEN 'd' THEN 'SET DEFAULT' ELSE 'NO ACTION' END \
                 FROM pg_constraint c \
                 JOIN pg_class cl ON cl.oid = c.conrelid JOIN pg_namespace cn ON cn.oid = cl.relnamespace \
                 JOIN pg_class pl ON pl.oid = c.confrelid JOIN pg_namespace pn ON pn.oid = pl.relnamespace \
                 CROSS JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(att, ref_att, pos) \
                 JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.att \
                 JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = k.ref_att \
                 WHERE c.contype = 'f' AND NOT cl.relispartition AND NOT pl.relispartition \
                 ORDER BY cn.nspname, cl.relname, c.conname, k.pos"
                .into(),
            SqlEngine::Mysql => "SELECT k.TABLE_SCHEMA, k.TABLE_NAME, k.CONSTRAINT_NAME, k.COLUMN_NAME, \
                 k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE \
                 FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
                 ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                 AND r.TABLE_NAME = k.TABLE_NAME \
                 WHERE k.REFERENCED_TABLE_NAME IS NOT NULL \
                 ORDER BY k.TABLE_SCHEMA, k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION"
                .into(),
            // A key without target columns points at the parent's primary key.
            SqlEngine::Sqlite => {
                let lit = self.literal(schema);
                format!(
                    "SELECT {lit}, m.name, m.name || '#' || f.id, f.\"from\", {lit}, f.\"table\", \
                     COALESCE(f.\"to\", (SELECT p.name FROM pragma_table_info(f.\"table\", {lit}) p WHERE p.pk = f.seq + 1)), \
                     f.on_delete \
                     FROM {}.sqlite_master m, pragma_foreign_key_list(m.name, {lit}) f \
                     WHERE m.type = 'table' ORDER BY m.name, f.id, f.seq",
                    self.ident(schema)
                )
            }
        }
    }

    // ---- browsing ------------------------------------------------------

    fn text_expr(&self, column: &str) -> String {
        match self.0 {
            SqlEngine::Postgres => format!("{}::text", self.ident(column)),
            _ => self.ident(column),
        }
    }

    fn filter_sql(&self, f: &Filter) -> Result<String> {
        let col = self.ident(&f.column);
        let like = match self.0 {
            SqlEngine::Postgres => "ILIKE",
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
        let mut order = Vec::new();
        if let Some(sort) = &req.sort {
            order.push(format!("{} {}", self.ident(&sort.column), if sort.desc { "DESC" } else { "ASC" }));
        }
        for key in &req.key_order {
            if req.sort.as_ref().is_none_or(|s| &s.column != key) {
                order.push(self.ident(key));
            }
        }
        if !order.is_empty() {
            sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
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
            SqlEngine::Postgres => (
                format!("ctid = (SELECT ctid FROM {table} WHERE {cond} LIMIT 1)"),
                "",
            ),
            SqlEngine::Sqlite => (
                format!("rowid = (SELECT rowid FROM {table} WHERE {cond} LIMIT 1)"),
                "",
            ),
            SqlEngine::Mysql => (cond.to_string(), " LIMIT 1"),
        }
    }

    /// SQLite matches table names without regard to case.
    pub fn same_table(&self, a: (&str, &str), b: (&str, &str)) -> bool {
        match self.0 {
            SqlEngine::Sqlite => a.0.eq_ignore_ascii_case(b.0) && a.1.eq_ignore_ascii_case(b.1),
            _ => a == b,
        }
    }

    /// `a` or `(a, b)`, for comparing a (possibly composite) key with IN.
    fn key_tuple(&self, columns: &[String]) -> String {
        let cols: Vec<_> = columns.iter().map(|c| self.ident(c)).collect();
        match cols.as_slice() {
            [one] => one.clone(),
            _ => format!("({})", cols.join(", ")),
        }
    }

    /// The rows `set` deletes, as a condition on its table.
    pub fn deleted_rows_filter(&self, set: &ChangeSet) -> Option<String> {
        let deleted: Vec<String> = set
            .changes
            .iter()
            .filter_map(|c| match c {
                RowChange::Delete { key } if !key.is_empty() => {
                    Some(format!("({})", self.match_clause(key, set.key_is_primary)))
                }
                _ => None,
            })
            .collect();
        (!deleted.is_empty()).then(|| deleted.join(" OR "))
    }

    /// The foreign keys pointing at the last table of `path`, except those
    /// coming from a table already on it (self-references, cycles).
    pub fn references_to<'a>(&self, fks: &'a [ForeignKey], path: &[(String, String)]) -> Vec<&'a ForeignKey> {
        let (schema, table) = path.last().expect("path starts at the edited table");
        fks.iter()
            .filter(|fk| {
                self.same_table((&fk.parent_schema, &fk.parent_table), (schema, table))
                    && !path.iter().any(|(s, t)| self.same_table((s, t), (&fk.schema, &fk.table)))
            })
            .collect()
    }

    /// The rows of `fk.table` pointing at the parent rows `parent_filter` selects.
    pub fn reference_step(&self, fk: &ForeignKey, parent_filter: &str, depth: u32) -> CascadeStep {
        let filter = format!(
            "{} IN (SELECT {} FROM {} WHERE {parent_filter})",
            self.key_tuple(&fk.columns),
            fk.parent_columns.iter().map(|c| self.ident(c)).collect::<Vec<_>>().join(", "),
            self.table(&fk.parent_schema, &fk.parent_table),
        );
        CascadeStep { fk: fk.clone(), depth, filter }
    }

    pub fn cascade_count_sql(&self, step: &CascadeStep) -> String {
        format!("SELECT COUNT(*) FROM {} WHERE {}", self.table(&step.fk.schema, &step.fk.table), step.filter)
    }

    /// Statements for `set`. The `cascade` steps' deletes run right before the
    /// first staged delete; steps the database resolves with SET NULL/DEFAULT
    /// are left to it.
    pub fn change_statements(&self, set: &ChangeSet, cascade: &[CascadeStep]) -> Result<Vec<String>> {
        let table = self.table(&set.schema, &set.table);
        let mut out = Vec::with_capacity(set.changes.len() + cascade.len());
        let mut cascade = cascade.iter();
        for change in &set.changes {
            match change {
                RowChange::Insert { values } => {
                    if values.is_empty() {
                        out.push(match self.0 {
                            SqlEngine::Mysql => format!("INSERT INTO {table} () VALUES ()"),
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
                    for step in cascade.by_ref().filter(|s| !s.fk.sets_null()) {
                        out.push(format!(
                            "DELETE FROM {} WHERE {}",
                            self.table(&step.fk.schema, &step.fk.table),
                            step.filter
                        ));
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

// ---- schema tools ------------------------------------------------------

/// Groups the rows of `Dialect::indexes_sql` (one per key column) into indexes.
pub fn group_indexes(rows: Vec<Vec<Cell>>) -> Vec<IndexInfo> {
    let mut out: Vec<IndexInfo> = Vec::new();
    for row in rows {
        let mut it = row.into_iter();
        let mut next = || it.next().flatten();
        let name = next().unwrap_or_default();
        // Expression columns have no name in MySQL and SQLite.
        let column = next().unwrap_or_else(|| "(expression)".into());
        let (unique, primary, constraint) = (next(), next(), next());
        let (method, definition) = (next(), next());
        match out.last_mut() {
            Some(ix) if ix.name == name => ix.columns.push(column),
            _ => out.push(IndexInfo {
                name,
                columns: vec![column],
                unique: unique.as_deref() == Some("YES"),
                primary: primary.as_deref() == Some("YES"),
                constraint: constraint.as_deref() == Some("YES"),
                method: method.map(|m| m.to_lowercase()),
                definition,
            }),
        }
    }
    out
}

impl Dialect {
    /// Rows of (index, column, unique, primary, backs a constraint ('YES'/'NO'),
    /// method, definition), one per key column in key order, primary key first.
    /// SQLite has no index for an INTEGER PRIMARY KEY: it is the rowid.
    pub fn indexes_sql(&self, schema: &str, table: &str) -> String {
        match self.0 {
            SqlEngine::Postgres => format!(
                "SELECT i.relname, COALESCE(a.attname, pg_get_indexdef(x.indexrelid, k.pos, true)), \
                 CASE WHEN x.indisunique THEN 'YES' ELSE 'NO' END, CASE WHEN x.indisprimary THEN 'YES' ELSE 'NO' END, \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = x.indexrelid \
                   AND c.conrelid = x.indrelid AND c.contype IN ('p','u','x')) THEN 'YES' ELSE 'NO' END, \
                 am.amname, pg_get_indexdef(x.indexrelid) \
                 FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_am am ON am.oid = i.relam \
                 CROSS JOIN LATERAL generate_series(1, x.indnkeyatts) AS k(pos) \
                 LEFT JOIN pg_attribute a ON a.attrelid = x.indrelid AND a.attnum = x.indkey[k.pos - 1] \
                 WHERE x.indrelid = {}::regclass \
                 ORDER BY x.indisprimary DESC, i.relname, k.pos",
                self.literal(&self.table(schema, table))
            ),
            SqlEngine::Mysql => format!(
                "SELECT INDEX_NAME, COLUMN_NAME, CASE WHEN NON_UNIQUE = 0 THEN 'YES' ELSE 'NO' END, \
                 CASE WHEN INDEX_NAME = 'PRIMARY' THEN 'YES' ELSE 'NO' END, \
                 CASE WHEN INDEX_NAME = 'PRIMARY' THEN 'YES' ELSE 'NO' END, INDEX_TYPE, NULL \
                 FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
                 ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
                self.literal(schema),
                self.literal(table)
            ),
            // Indexes SQLite creates for PRIMARY KEY/UNIQUE ("pk"/"u") have no SQL.
            SqlEngine::Sqlite => {
                let lit = self.literal(schema);
                format!(
                    "SELECT l.name, i.name, CASE WHEN l.\"unique\" THEN 'YES' ELSE 'NO' END, \
                     CASE WHEN l.origin = 'pk' THEN 'YES' ELSE 'NO' END, \
                     CASE WHEN l.origin IN ('pk','u') THEN 'YES' ELSE 'NO' END, NULL, m.sql \
                     FROM pragma_index_list({}, {lit}) l, pragma_index_info(l.name, {lit}) i \
                     LEFT JOIN {}.sqlite_master m ON m.type = 'index' AND m.name = l.name \
                     ORDER BY l.origin = 'pk' DESC, l.name, i.seqno",
                    self.literal(table),
                    self.ident(schema)
                )
            }
        }
    }

    /// MySQL: `SHOW CREATE TABLE`, (name, statement). SQLite: the stored
    /// statements of the table and its indexes and triggers. Postgres has no
    /// such thing; this is one row of (relkind, persistence, view definition,
    /// partition key) for `pg_ddl`.
    pub fn ddl_sql(&self, schema: &str, table: &str) -> String {
        match self.0 {
            SqlEngine::Postgres => format!(
                "SELECT c.relkind, c.relpersistence, \
                 CASE WHEN c.relkind IN ('v','m') THEN pg_get_viewdef(c.oid, true) END, \
                 CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END \
                 FROM pg_class c WHERE c.oid = {}::regclass",
                self.literal(&self.table(schema, table))
            ),
            SqlEngine::Mysql => format!("SHOW CREATE TABLE {}", self.table(schema, table)),
            SqlEngine::Sqlite => format!(
                "SELECT sql FROM {}.sqlite_master WHERE tbl_name = {} COLLATE NOCASE AND sql IS NOT NULL \
                 ORDER BY CASE type WHEN 'index' THEN 1 WHEN 'trigger' THEN 2 ELSE 0 END, name",
                self.ident(schema),
                self.literal(table)
            ),
        }
    }

    /// Postgres rows of (name, type, NOT NULL 'YES'/'NO', default or
    /// generation expression, identity 'a'/'d'/'', generated 's'/'v'/'').
    pub fn pg_ddl_columns_sql(&self, schema: &str, table: &str) -> String {
        format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'YES' ELSE 'NO' END, \
             pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text \
             FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            self.literal(&self.table(schema, table))
        )
    }

    /// Postgres rows of (name, definition): primary key, unique, check,
    /// foreign key and exclusion constraints, in that order.
    pub fn pg_ddl_constraints_sql(&self, schema: &str, table: &str) -> String {
        format!(
            "SELECT conname, pg_get_constraintdef(oid, true) FROM pg_constraint \
             WHERE conrelid = {}::regclass AND contype IN ('p','u','c','f','x') \
             ORDER BY CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'c' THEN 2 WHEN 'f' THEN 3 ELSE 4 END, conname",
            self.literal(&self.table(schema, table))
        )
    }

    /// A readable CREATE TABLE (or VIEW) for Postgres from the rows of
    /// `ddl_sql`, `pg_ddl_columns_sql` and `pg_ddl_constraints_sql`, followed
    /// by the indexes that don't back a constraint.
    pub fn pg_ddl(
        &self,
        schema: &str,
        table: &str,
        head: &[Cell],
        columns: &[Vec<Cell>],
        constraints: &[Vec<Cell>],
        indexes: &[IndexInfo],
    ) -> String {
        let get = |row: &[Cell], i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let name = self.table(schema, table);
        let kind = get(head, 0);
        let mut out = if let Some(def) = head.get(2).cloned().flatten() {
            let view = if kind == "m" { "MATERIALIZED VIEW" } else { "VIEW" };
            format!("CREATE {view} {name} AS\n{};", def.trim_end().trim_end_matches(';'))
        } else {
            let mut lines = Vec::with_capacity(columns.len() + constraints.len());
            for col in columns {
                let mut line = format!("  {} {}", self.ident(&get(col, 0)), get(col, 1));
                let default = get(col, 3);
                match (get(col, 4).as_str(), get(col, 5).as_str()) {
                    (_, "s") => line.push_str(&format!(" GENERATED ALWAYS AS ({default}) STORED")),
                    (_, "v") => line.push_str(&format!(" GENERATED ALWAYS AS ({default}) VIRTUAL")),
                    ("a", _) => line.push_str(" GENERATED ALWAYS AS IDENTITY"),
                    ("d", _) => line.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
                    _ if !default.is_empty() => line.push_str(&format!(" DEFAULT {default}")),
                    _ => {}
                }
                if get(col, 2) == "YES" {
                    line.push_str(" NOT NULL");
                }
                lines.push(line);
            }
            for c in constraints {
                lines.push(format!("  CONSTRAINT {} {}", self.ident(&get(c, 0)), get(c, 1)));
            }
            let unlogged = if get(head, 1) == "u" { "UNLOGGED " } else { "" };
            let mut sql = format!("CREATE {unlogged}TABLE {name} (\n{}\n)", lines.join(",\n"));
            if let Some(key) = head.get(3).cloned().flatten() {
                sql.push_str(&format!(" PARTITION BY {key}"));
            }
            sql.push(';');
            sql
        };
        for def in indexes.iter().filter(|i| !i.constraint).filter_map(|i| i.definition.as_ref()) {
            out.push_str(&format!("\n\n{def};"));
        }
        out
    }

    /// The statements that apply `change` to `schema.table`.
    pub fn schema_change_sql(&self, schema: &str, table: &str, change: &SchemaChange) -> Result<Vec<String>> {
        fn required<'a>(what: &str, value: &'a str) -> Result<&'a str> {
            match value.trim() {
                "" => bail!("enter {what}"),
                v => Ok(v),
            }
        }
        let target = self.table(schema, table);
        Ok(vec![match change {
            SchemaChange::AddColumn { name, data_type, nullable, default } => {
                let mut sql = format!(
                    "ALTER TABLE {target} ADD COLUMN {} {}",
                    self.ident(required("a column name", name)?),
                    required("a column type", data_type)?
                );
                if let Some(default) = default.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                    sql.push_str(&format!(" DEFAULT {default}"));
                }
                if !nullable {
                    sql.push_str(" NOT NULL");
                }
                sql
            }
            SchemaChange::AddIndex { name, columns, unique } => {
                let name = required("an index name", name)?;
                if columns.is_empty() {
                    bail!("choose at least one column");
                }
                // SQLite qualifies the index with the schema, the others the table.
                let (index, on) = match self.0 {
                    SqlEngine::Sqlite => (self.table(schema, name), self.ident(table)),
                    _ => (self.ident(name), target),
                };
                let cols: Vec<_> = columns.iter().map(|c| self.ident(c)).collect();
                format!(
                    "CREATE {}INDEX {index} ON {on} ({})",
                    if *unique { "UNIQUE " } else { "" },
                    cols.join(", ")
                )
            }
            SchemaChange::DropIndex { name, constraint } => {
                let name = required("an index name", name)?;
                match self.0 {
                    SqlEngine::Postgres if *constraint => {
                        format!("ALTER TABLE {target} DROP CONSTRAINT {}", self.ident(name))
                    }
                    SqlEngine::Sqlite if *constraint => {
                        bail!("{name} belongs to a PRIMARY KEY or UNIQUE constraint of the table; SQLite can't drop it")
                    }
                    SqlEngine::Postgres | SqlEngine::Sqlite => format!("DROP INDEX {}", self.table(schema, name)),
                    SqlEngine::Mysql if name == "PRIMARY" => format!("ALTER TABLE {target} DROP PRIMARY KEY"),
                    SqlEngine::Mysql => format!("DROP INDEX {} ON {target}", self.ident(name)),
                }
            }
        }])
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
        let pg = Dialect(SqlEngine::Postgres);
        let my = Dialect(SqlEngine::Mysql);
        assert_eq!(pg.ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(my.ident("we`ird"), "`we``ird`");
        assert_eq!(pg.literal("it's \\"), "'it''s \\'");
        assert_eq!(my.literal("it's \\\n"), "'it''s \\\\\\n'");
    }

    #[test]
    fn page_query() {
        let pg = Dialect(SqlEngine::Postgres);
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
            sort: Some(Sort { column: "name".into(), desc: true }),
            key_order: vec!["id".into()],
        };
        assert_eq!(
            pg.select_page_sql(&req).unwrap(),
            "SELECT * FROM \"public\".\"users\" WHERE \"name\"::text ILIKE '%ann%' AND \"deleted_at\" IS NULL ORDER BY \"name\" DESC, \"id\" LIMIT 100 OFFSET 200"
        );
        let unsorted = FetchRequest { sort: None, filters: vec![], ..req };
        assert_eq!(
            pg.select_page_sql(&unsorted).unwrap(),
            "SELECT * FROM \"public\".\"users\" ORDER BY \"id\" LIMIT 100 OFFSET 200"
        );
    }

    #[test]
    fn changes() {
        let my = Dialect(SqlEngine::Mysql);
        let set = ChangeSet {
            schema: "shop".into(),
            table: "items".into(),
            key_is_primary: true,
            cascade: false,
            changes: vec![
                RowChange::Update { key: vec![cv("id", Some("7"))], values: vec![cv("name", Some("O'Brien")), cv("note", None)] },
                RowChange::Insert { values: vec![cv("name", Some("x"))] },
                RowChange::Delete { key: vec![cv("id", Some("9"))] },
            ],
        };
        assert_eq!(
            my.change_statements(&set, &[]).unwrap(),
            vec![
                "UPDATE `shop`.`items` SET `name` = 'O''Brien', `note` = NULL WHERE `id` = '7'",
                "INSERT INTO `shop`.`items` (`name`) VALUES ('x')",
                "DELETE FROM `shop`.`items` WHERE `id` = '9'",
            ]
        );

        let pg = Dialect(SqlEngine::Postgres);
        let set = ChangeSet {
            schema: "public".into(),
            table: "logs".into(),
            key_is_primary: false,
            cascade: false,
            changes: vec![RowChange::Delete { key: vec![cv("msg", Some("a")), cv("at", None)] }],
        };
        assert_eq!(
            pg.change_statements(&set, &[]).unwrap(),
            vec!["DELETE FROM \"public\".\"logs\" WHERE ctid = (SELECT ctid FROM \"public\".\"logs\" WHERE \"msg\"::text = 'a' AND \"at\"::text IS NULL LIMIT 1)"]
        );
    }

    fn fk(table: &str, cols: &[&str], parent: &str, parent_cols: &[&str]) -> ForeignKey {
        ForeignKey {
            schema: "public".into(),
            table: table.into(),
            name: format!("{table}_fk"),
            columns: cols.iter().map(|c| c.to_string()).collect(),
            parent_schema: "public".into(),
            parent_table: parent.into(),
            parent_columns: parent_cols.iter().map(|c| c.to_string()).collect(),
            on_delete: "NO ACTION".into(),
        }
    }

    #[test]
    fn foreign_keys_group_by_constraint() {
        let row = |c: &str, p: &str| -> Vec<Cell> {
            ["s", "lines", "fk1", c, "s", "orders", p, "cascade"].iter().map(|v| Some(v.to_string())).collect()
        };
        let fks = ForeignKey::from_rows(vec![row("order_id", "id"), row("shop", "shop")]);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].columns, ["order_id", "shop"]);
        assert_eq!(fks[0].parent_columns, ["id", "shop"]);
        assert_eq!(fks[0].on_delete, "CASCADE");
    }

    #[test]
    fn cascade_steps_follow_references() {
        let pg = Dialect(SqlEngine::Postgres);
        let fks = vec![
            fk("orders", &["customer_id"], "customers", &["id"]),
            fk("lines", &["order_id", "shop"], "orders", &["id", "shop"]),
            // Self-reference and a cycle back to the start aren't followed.
            fk("customers", &["referred_by"], "customers", &["id"]),
            fk("customers", &["last_order"], "orders", &["id"]),
            ForeignKey { on_delete: "SET NULL".into(), ..fk("notes", &["order_id"], "orders", &["id"]) },
        ];
        let mut path = vec![("public".to_string(), "customers".to_string())];
        let names = |refs: Vec<&ForeignKey>| refs.into_iter().map(|f| f.table.clone()).collect::<Vec<_>>();
        assert_eq!(names(pg.references_to(&fks, &path)), ["orders"]);
        path.push(("public".into(), "orders".into()));
        assert_eq!(names(pg.references_to(&fks, &path)), ["lines", "notes"]);

        let set = ChangeSet {
            schema: "public".into(),
            table: "customers".into(),
            key_is_primary: true,
            cascade: true,
            changes: vec![
                RowChange::Update { key: vec![cv("id", Some("1"))], values: vec![cv("name", Some("x"))] },
                RowChange::Delete { key: vec![cv("id", Some("2"))] },
                RowChange::Delete { key: vec![cv("id", Some("3"))] },
            ],
        };
        let root = pg.deleted_rows_filter(&set).unwrap();
        assert_eq!(root, "(\"id\" = '2') OR (\"id\" = '3')");
        let orders = pg.reference_step(&fks[0], &root, 1);
        let lines = pg.reference_step(&fks[1], &orders.filter, 2);
        let notes = pg.reference_step(&fks[4], &orders.filter, 2);
        let o = "\"customer_id\" IN (SELECT \"id\" FROM \"public\".\"customers\" WHERE (\"id\" = '2') OR (\"id\" = '3'))";
        assert_eq!(orders.filter, o);
        assert_eq!(pg.cascade_count_sql(&orders), format!("SELECT COUNT(*) FROM \"public\".\"orders\" WHERE {o}"));
        // Deepest first; the SET NULL step is left to the database.
        assert_eq!(
            pg.change_statements(&set, &[notes, lines, orders]).unwrap(),
            vec![
                "UPDATE \"public\".\"customers\" SET \"name\" = 'x' WHERE \"id\" = '1'".to_string(),
                format!("DELETE FROM \"public\".\"lines\" WHERE (\"order_id\", \"shop\") IN (SELECT \"id\", \"shop\" FROM \"public\".\"orders\" WHERE {o})"),
                format!("DELETE FROM \"public\".\"orders\" WHERE {o}"),
                "DELETE FROM \"public\".\"customers\" WHERE \"id\" = '2'".to_string(),
                "DELETE FROM \"public\".\"customers\" WHERE \"id\" = '3'".to_string(),
            ]
        );
    }


    fn cells(values: &[Option<&str>]) -> Vec<Cell> {
        values.iter().map(|v| v.map(Into::into)).collect()
    }

    #[test]
    fn index_queries() {
        let pg = Dialect(SqlEngine::Postgres);
        let sql = pg.indexes_sql("public", "users");
        assert!(sql.contains("WHERE x.indrelid = '\"public\".\"users\"'::regclass"), "{sql}");
        assert!(sql.contains("generate_series(1, x.indnkeyatts)"), "{sql}");
        let my = Dialect(SqlEngine::Mysql);
        let sql = my.indexes_sql("shop", "it's");
        assert!(sql.contains("TABLE_SCHEMA = 'shop' AND TABLE_NAME = 'it''s'"), "{sql}");
        let lt = Dialect(SqlEngine::Sqlite);
        let sql = lt.indexes_sql("main", "pets");
        assert!(sql.contains("FROM pragma_index_list('pets', 'main') l, pragma_index_info(l.name, 'main') i"), "{sql}");
        assert!(sql.contains("LEFT JOIN \"main\".sqlite_master m"), "{sql}");
    }

    #[test]
    fn indexes_group_by_name() {
        let rows = vec![
            cells(&[Some("users_pkey"), Some("id"), Some("YES"), Some("YES"), Some("YES"), Some("btree"), Some("CREATE UNIQUE INDEX users_pkey ON public.users USING btree (id)")]),
            cells(&[Some("users_name_idx"), Some("last"), Some("NO"), Some("NO"), Some("NO"), Some("BTREE"), None]),
            cells(&[Some("users_name_idx"), Some("first"), Some("NO"), Some("NO"), Some("NO"), Some("BTREE"), None]),
            cells(&[Some("by_lower"), None, Some("NO"), Some("NO"), Some("NO"), None, None]),
        ];
        let ix = group_indexes(rows);
        assert_eq!(ix.len(), 3);
        assert!(ix[0].primary && ix[0].unique && ix[0].constraint);
        assert_eq!(ix[1].columns, ["last", "first"]);
        assert_eq!(ix[1].method.as_deref(), Some("btree"));
        assert!(!ix[1].unique && ix[1].definition.is_none());
        assert_eq!(ix[2].columns, ["(expression)"]);
    }

    #[test]
    fn ddl_queries() {
        assert_eq!(Dialect(SqlEngine::Mysql).ddl_sql("shop", "items"), "SHOW CREATE TABLE `shop`.`items`");
        let sql = Dialect(SqlEngine::Sqlite).ddl_sql("main", "pets");
        assert!(sql.starts_with("SELECT sql FROM \"main\".sqlite_master WHERE tbl_name = 'pets'"), "{sql}");
        let pg = Dialect(SqlEngine::Postgres);
        assert!(pg.ddl_sql("public", "users").contains("WHERE c.oid = '\"public\".\"users\"'::regclass"));
        assert!(pg.pg_ddl_columns_sql("public", "users").contains("a.attidentity::text"));
        assert!(pg.pg_ddl_constraints_sql("public", "users").contains("contype IN ('p','u','c','f','x')"));
    }

    #[test]
    fn postgres_create_table() {
        let pg = Dialect(SqlEngine::Postgres);
        let head = cells(&[Some("r"), Some("p"), None, None]);
        let columns = vec![
            cells(&[Some("id"), Some("integer"), Some("YES"), None, Some("a"), Some("")]),
            cells(&[Some("email"), Some("text"), Some("NO"), None, Some(""), Some("")]),
            cells(&[Some("created"), Some("timestamp with time zone"), Some("YES"), Some("now()"), Some(""), Some("")]),
            cells(&[Some("domain"), Some("text"), Some("NO"), Some("split_part(email, '@'::text, 2)"), Some(""), Some("s")]),
        ];
        let constraints = vec![
            cells(&[Some("users_pkey"), Some("PRIMARY KEY (id)")]),
            cells(&[Some("users_email_key"), Some("UNIQUE (email)")]),
            cells(&[Some("users_team_fkey"), Some("FOREIGN KEY (team) REFERENCES teams(id) ON DELETE CASCADE")]),
        ];
        let index = |name: &str, constraint: bool, def: &str| IndexInfo {
            name: name.into(),
            columns: vec!["x".into()],
            unique: constraint,
            primary: false,
            constraint,
            method: Some("btree".into()),
            definition: Some(def.into()),
        };
        let indexes = vec![
            index("users_email_key", true, "CREATE UNIQUE INDEX users_email_key ON public.users USING btree (email)"),
            index("users_created_idx", false, "CREATE INDEX users_created_idx ON public.users USING btree (created)"),
        ];
        assert_eq!(
            pg.pg_ddl("public", "users", &head, &columns, &constraints, &indexes),
            "CREATE TABLE \"public\".\"users\" (\n\
             \x20 \"id\" integer GENERATED ALWAYS AS IDENTITY NOT NULL,\n\
             \x20 \"email\" text,\n\
             \x20 \"created\" timestamp with time zone DEFAULT now() NOT NULL,\n\
             \x20 \"domain\" text GENERATED ALWAYS AS (split_part(email, '@'::text, 2)) STORED,\n\
             \x20 CONSTRAINT \"users_pkey\" PRIMARY KEY (id),\n\
             \x20 CONSTRAINT \"users_email_key\" UNIQUE (email),\n\
             \x20 CONSTRAINT \"users_team_fkey\" FOREIGN KEY (team) REFERENCES teams(id) ON DELETE CASCADE\n\
             );\n\n\
             CREATE INDEX users_created_idx ON public.users USING btree (created);"
        );
        let partitioned = cells(&[Some("p"), Some("u"), None, Some("RANGE (created)")]);
        assert_eq!(
            pg.pg_ddl("public", "events", &partitioned, &columns[..1], &[], &[]),
            "CREATE UNLOGGED TABLE \"public\".\"events\" (\n  \"id\" integer GENERATED ALWAYS AS IDENTITY NOT NULL\n) PARTITION BY RANGE (created);"
        );
        let view = cells(&[Some("v"), Some("p"), Some(" SELECT id\n   FROM users;"), None]);
        assert_eq!(
            pg.pg_ddl("public", "ids", &view, &[], &[], &[]),
            "CREATE VIEW \"public\".\"ids\" AS\n SELECT id\n   FROM users;"
        );
    }

    #[test]
    fn schema_changes() {
        let column = |name: &str, data_type: &str, nullable: bool, default: Option<&str>| SchemaChange::AddColumn {
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: default.map(Into::into),
        };
        let add_column = column("note", "varchar(40)", false, Some(" 'n/a' "));
        let add_index = SchemaChange::AddIndex { name: "pets_owner_idx".into(), columns: vec!["owner".into(), "na\"me".into()], unique: true };
        let pg = Dialect(SqlEngine::Postgres);
        let my = Dialect(SqlEngine::Mysql);
        let lt = Dialect(SqlEngine::Sqlite);
        assert_eq!(
            pg.schema_change_sql("public", "pets", &add_column).unwrap(),
            ["ALTER TABLE \"public\".\"pets\" ADD COLUMN \"note\" varchar(40) DEFAULT 'n/a' NOT NULL"]
        );
        assert_eq!(
            my.schema_change_sql("zoo", "pets", &column("note", "varchar(40)", true, Some(""))).unwrap(),
            ["ALTER TABLE `zoo`.`pets` ADD COLUMN `note` varchar(40)"]
        );
        assert_eq!(
            pg.schema_change_sql("public", "pets", &add_index).unwrap(),
            ["CREATE UNIQUE INDEX \"pets_owner_idx\" ON \"public\".\"pets\" (\"owner\", \"na\"\"me\")"]
        );
        assert_eq!(
            my.schema_change_sql("zoo", "pets", &add_index).unwrap(),
            ["CREATE UNIQUE INDEX `pets_owner_idx` ON `zoo`.`pets` (`owner`, `na\"me`)"]
        );
        assert_eq!(
            lt.schema_change_sql("main", "pets", &add_index).unwrap(),
            ["CREATE UNIQUE INDEX \"main\".\"pets_owner_idx\" ON \"pets\" (\"owner\", \"na\"\"me\")"]
        );

        let drop = |name: &str, constraint: bool| SchemaChange::DropIndex { name: name.into(), constraint };
        assert_eq!(pg.schema_change_sql("public", "pets", &drop("pets_owner_idx", false)).unwrap(), ["DROP INDEX \"public\".\"pets_owner_idx\""]);
        assert_eq!(
            pg.schema_change_sql("public", "pets", &drop("pets_name_key", true)).unwrap(),
            ["ALTER TABLE \"public\".\"pets\" DROP CONSTRAINT \"pets_name_key\""]
        );
        assert_eq!(my.schema_change_sql("zoo", "pets", &drop("by_owner", false)).unwrap(), ["DROP INDEX `by_owner` ON `zoo`.`pets`"]);
        assert_eq!(my.schema_change_sql("zoo", "pets", &drop("PRIMARY", true)).unwrap(), ["ALTER TABLE `zoo`.`pets` DROP PRIMARY KEY"]);
        assert_eq!(lt.schema_change_sql("main", "pets", &drop("by_owner", false)).unwrap(), ["DROP INDEX \"main\".\"by_owner\""]);
        assert!(lt.schema_change_sql("main", "pets", &drop("sqlite_autoindex_pets_1", true)).is_err());

        // Missing pieces are refused rather than turned into broken SQL.
        assert!(pg.schema_change_sql("public", "pets", &column(" ", "text", true, None)).is_err());
        assert!(pg.schema_change_sql("public", "pets", &column("note", "", true, None)).is_err());
        assert!(pg.schema_change_sql("public", "pets", &SchemaChange::AddIndex { name: "i".into(), columns: vec![], unique: false }).is_err());
    }
}
