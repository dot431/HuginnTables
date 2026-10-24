//! SQL generation for each supported engine: identifier quoting, literals,
//! catalog queries, table paging and staged-change statements.

use anyhow::{bail, Result};

use crate::model::{Cell, CellValue, ChangeSet, DriverKind, FetchRequest, Filter, RowChange};

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
    fn same_table(&self, a: (&str, &str), b: (&str, &str)) -> bool {
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
}
