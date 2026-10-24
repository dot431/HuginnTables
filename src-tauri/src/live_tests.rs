//! End-to-end tests against real servers. They are ignored by default; start
//! the containers described in `scripts/test-db.sh` and run
//! `cargo test -- --ignored --test-threads=1`.

use std::time::{Duration, Instant};

use crate::model::*;
use crate::session::Session;

fn base(driver: DriverKind) -> ConnectionConfig {
    ConnectionConfig {
        id: String::new(),
        name: "test".into(),
        driver,
        color: String::new(),
        template: String::new(),
        host: "127.0.0.1".into(),
        port: 0,
        user: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Prefer,
        file_path: String::new(),
        uri: String::new(),
        ssh: SshConfig::default(),
        production: false,
        read_only: false,
        password: Some("huginn".into()),
        ssh_password: None,
        ssh_passphrase: None,
    }
}

fn pg() -> ConnectionConfig {
    ConnectionConfig { port: 55432, user: "postgres".into(), ..base(DriverKind::Postgres) }
}

fn mysql() -> ConnectionConfig {
    ConnectionConfig {
        port: 53306,
        user: "root".into(),
        database: "shop".into(),
        ..base(DriverKind::Mysql)
    }
}

fn cv(column: &str, value: Option<&str>) -> CellValue {
    CellValue { column: column.into(), value: value.map(Into::into) }
}

async fn exec(s: &Session, sql: &[&str]) {
    let results = s.run_statements(&sql.iter().map(|s| s.to_string()).collect::<Vec<_>>()).await;
    for r in &results {
        assert!(r.error.is_none(), "{}: {:?}", r.sql, r.error);
    }
}

/// Shared scenario: browse, filter, page, stage changes, commit and roll back.
async fn exercise(s: &Session, schema: &str) {
    let tables = s.list_tables(schema).await.unwrap();
    assert!(tables.iter().any(|t| t.name == "people" && t.kind == "table"), "{tables:?}");
    assert!(tables.iter().any(|t| t.name == "people_view" && t.kind == "view"), "{tables:?}");

    let cols = s.table_columns(schema, "people").await.unwrap();
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name", "note"]);
    assert!(cols[0].is_primary_key && !cols[1].is_primary_key);
    assert!(!cols[1].nullable && cols[2].nullable);

    let completion = s.schema_columns(schema).await.unwrap();
    assert_eq!(completion["people"], ["id", "name", "note"]);

    let mut req = FetchRequest {
        schema: schema.into(),
        table: "people".into(),
        limit: 2,
        offset: 1,
        filters: vec![],
        raw_where: String::new(),
        sort: Some(Sort { column: "id".into(), desc: false }),
        key_order: vec!["id".into()],
    };
    let page = s.fetch_rows(&req).await.unwrap();
    assert_eq!(page.rows, vec![
        vec![Some("2".into()), Some("Bob".into()), None],
        vec![Some("3".into()), Some("O'Hara \\ x".into()), Some("q".into())],
    ]);
    assert_eq!(s.count_rows(&req).await.unwrap(), 3);

    req.filters = vec![Filter { column: "name".into(), op: "contains".into(), value: "ha".into() }];
    req.offset = 0;
    assert_eq!(s.count_rows(&req).await.unwrap(), 1);
    req.filters = vec![Filter { column: "note".into(), op: "isNull".into(), value: String::new() }];
    assert_eq!(s.fetch_rows(&req).await.unwrap().rows.len(), 2);

    let set = ChangeSet {
        schema: schema.into(),
        table: "people".into(),
        key_is_primary: true,
        cascade: false,
        changes: vec![
            RowChange::Update { key: vec![cv("id", Some("1"))], values: vec![cv("name", Some("Ann 'B' \\ \n"))] },
            RowChange::Delete { key: vec![cv("id", Some("2"))] },
            RowChange::Insert { values: vec![cv("id", Some("10")), cv("name", Some("Zed")), cv("note", None)] },
        ],
    };
    assert_eq!(s.commit_changes(&set).await.unwrap(), 3);

    req.filters.clear();
    req.limit = 100;
    let rows = s.fetch_rows(&req).await.unwrap().rows;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1].as_deref(), Some("Ann 'B' \\ \n"));
    assert_eq!(rows[2][0].as_deref(), Some("10"));

    // A failing statement rolls back the whole change set.
    let bad = ChangeSet {
        schema: schema.into(),
        table: "people".into(),
        key_is_primary: true,
        cascade: false,
        changes: vec![
            RowChange::Update { key: vec![cv("id", Some("1"))], values: vec![cv("name", Some("changed"))] },
            RowChange::Update { key: vec![cv("id", Some("3"))], values: vec![cv("name", None)] },
        ],
    };
    assert!(s.commit_changes(&bad).await.is_err());
    let rows = s.fetch_rows(&req).await.unwrap().rows;
    assert_eq!(rows[0][1].as_deref(), Some("Ann 'B' \\ \n"));

    // Tables without a primary key: only one of two identical rows is touched.
    let dupes = ChangeSet {
        schema: schema.into(),
        table: "nokey".into(),
        key_is_primary: false,
        cascade: false,
        changes: vec![RowChange::Delete { key: vec![cv("a", Some("1")), cv("b", None)] }],
    };
    assert_eq!(s.commit_changes(&dupes).await.unwrap(), 1);
    let left = s.run_statements(&["SELECT COUNT(*) FROM nokey".into()]).await;
    assert_eq!(left[0].rows[0][0].as_deref(), Some("1"));

    // Errors surface as results and stop the batch.
    let results = s
        .run_statements(&["SELECT 1 AS one".into(), "SELECT * FROM missing_table".into(), "SELECT 2".into()])
        .await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].columns[0].name, "one");
    assert!(results[1].error.is_some());
}

/// Structure tab, foreign-key navigation and export against a real server.
/// Runs after `cascade`, whose owners/ravens/sightings/nests tables it reads.
async fn features(s: &Session, schema: &str) {
    // Foreign keys around ravens: its own key to owners, and two pointing at it.
    let fks = s.table_foreign_keys(schema, "ravens").await.unwrap();
    assert_eq!(fks.outgoing.len(), 1, "{fks:?}");
    assert_eq!((fks.outgoing[0].parent_table.as_str(), &fks.outgoing[0].columns), ("owners", &vec!["clan".to_string(), "owner".to_string()]));
    assert_eq!(fks.outgoing[0].parent_columns, ["clan", "name"]);
    let mut incoming: Vec<_> = fks.incoming.iter().map(|f| (f.table.as_str(), f.on_delete.as_str())).collect();
    incoming.sort();
    assert_eq!(incoming, [("nests", "SET NULL"), ("sightings", "CASCADE")]);

    // Indexes: the composite primary key of owners.
    let ix = s.table_indexes(schema, "owners").await.unwrap();
    let pk = ix.iter().find(|i| i.primary).unwrap_or_else(|| panic!("no primary index: {ix:?}"));
    assert_eq!(pk.columns, ["clan", "name"]);
    assert!(pk.unique);

    // Add a column and an index, then drop the index, all through the generated SQL.
    let run = |sql: Vec<String>| async move {
        for r in s.run_statements(&sql).await {
            assert!(r.error.is_none(), "{}: {:?}", r.sql, r.error);
        }
    };
    let add_col = SchemaChange::AddColumn { name: "nick".into(), data_type: "varchar(20)".into(), nullable: true, default: Some("'none'".into()) };
    run(s.schema_change_sql(schema, "ravens", &add_col).await.unwrap()).await;
    let cols = s.table_columns(schema, "ravens").await.unwrap();
    let nick = cols.iter().find(|c| c.name == "nick").expect("nick column added");
    assert!(nick.nullable);
    let add_ix = SchemaChange::AddIndex { name: "ravens_nick_idx".into(), columns: vec!["nick".into(), "id".into()], unique: false };
    run(s.schema_change_sql(schema, "ravens", &add_ix).await.unwrap()).await;
    let ix = s.table_indexes(schema, "ravens").await.unwrap();
    let nick_ix = ix.iter().find(|i| i.name == "ravens_nick_idx").unwrap_or_else(|| panic!("{ix:?}"));
    assert_eq!(nick_ix.columns, ["nick", "id"]);
    assert!(!nick_ix.unique && !nick_ix.constraint);
    let drop_ix = SchemaChange::DropIndex { name: "ravens_nick_idx".into(), constraint: false };
    run(s.schema_change_sql(schema, "ravens", &drop_ix).await.unwrap()).await;
    assert!(!s.table_indexes(schema, "ravens").await.unwrap().iter().any(|i| i.name == "ravens_nick_idx"));

    // DDL round trip: drop the table and recreate it from its own DDL.
    exec(s, &[
        "DROP TABLE IF EXISTS probe",
        "CREATE TABLE probe (id int PRIMARY KEY, name varchar(30) NOT NULL DEFAULT 'x', score int CHECK (score > 0), owner_clan varchar(20), owner_name varchar(20), \
         FOREIGN KEY (owner_clan, owner_name) REFERENCES owners (clan, name))",
        "CREATE INDEX probe_name_idx ON probe (name)",
    ]).await;
    let ddl = s.table_ddl(schema, "probe").await.unwrap();
    let before = s.table_columns(schema, "probe").await.unwrap();
    exec(s, &["DROP TABLE probe"]).await;
    let statements: Vec<String> = ddl.split(";\n").map(|p| p.trim().trim_end_matches(';').to_string()).filter(|p| !p.is_empty()).collect();
    run(statements).await;
    let after = s.table_columns(schema, "probe").await.unwrap();
    let shape = |c: &[ColumnInfo]| c.iter().map(|c| (c.name.clone(), c.nullable, c.is_primary_key)).collect::<Vec<_>>();
    assert_eq!(shape(&before), shape(&after), "DDL:\n{ddl}");
    assert!(s.table_indexes(schema, "probe").await.unwrap().iter().any(|i| i.name == "probe_name_idx"), "DDL:\n{ddl}");
    assert_eq!(s.table_foreign_keys(schema, "probe").await.unwrap().outgoing.len(), 1, "DDL:\n{ddl}");

    // Export pages through the table: 10,000 rows, filtered to 9,900, over two 5,000-row pages.
    exec(s, &[
        "DROP TABLE IF EXISTS bulk",
        "DROP TABLE IF EXISTS digits",
        "CREATE TABLE digits (d int)",
        "INSERT INTO digits VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)",
        "CREATE TABLE bulk (id int PRIMARY KEY, v varchar(10))",
        "INSERT INTO bulk SELECT a.d + 10*b.d + 100*c.d + 1000*e.d + 1, 'it''s' FROM digits a, digits b, digits c, digits e",
    ]).await;
    let path = std::env::temp_dir().join(format!("huginn-export-{}.csv", uuid::Uuid::new_v4()));
    let req = crate::export::ExportRequest {
        schema: schema.into(),
        table: "bulk".into(),
        filters: vec![Filter { column: "id".into(), op: ">".into(), value: "100".into() }],
        raw_where: String::new(),
        sort: None,
        key_order: vec!["id".into()],
    };
    let n = crate::export::write_table(s, &req, crate::export::ExportFormat::Csv, &path.to_string_lossy()).await.unwrap();
    assert_eq!(n, 9900);
    let csv = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[1], lines[9900]), (9901, "id,v", "101,it's", "10000,it's"));
    let _ = std::fs::remove_file(path);
}

/// A read-only connection refuses writes from the grid and the editor.
async fn read_only(mut cfg: ConnectionConfig) {
    cfg.read_only = true;
    let s = Session::open("ro".into(), cfg).await.unwrap();
    assert!(s.info().await.read_only);
    let r = s.run_statements(&["SELECT COUNT(*) FROM owners".into()]).await;
    assert!(r[0].error.is_none(), "{:?}", r[0].error);
    for sql in ["INSERT INTO owners VALUES ('x', 'y')", "CREATE TABLE ro_probe (a int)", "DROP TABLE owners"] {
        let r = s.run_statements(&[sql.into()]).await;
        assert!(r[0].error.is_some(), "{sql} ran on a read-only connection");
    }
    let set = ChangeSet {
        schema: s.info().await.schema,
        table: "owners".into(),
        key_is_primary: true,
        cascade: false,
        changes: vec![RowChange::Insert { values: vec![cv("clan", Some("x")), cv("name", Some("y"))] }],
    };
    assert!(s.commit_changes(&set).await.is_err());
    s.close().await;
}

/// Deleting rows other tables point at (through a composite key, then a
/// single-column one): they're reported, a plain commit is refused and a
/// cascading one removes them, deepest first.
async fn cascade(s: &Session, schema: &str) {
    exec(s, &[
        "DROP TABLE IF EXISTS nests",
        "DROP TABLE IF EXISTS sightings",
        "DROP TABLE IF EXISTS ravens",
        "DROP TABLE IF EXISTS owners",
        "CREATE TABLE owners (clan varchar(20), name varchar(20), PRIMARY KEY (clan, name))",
        "CREATE TABLE ravens (id int PRIMARY KEY, clan varchar(20), owner varchar(20), \
         FOREIGN KEY (clan, owner) REFERENCES owners (clan, name))",
        "CREATE TABLE sightings (raven int NOT NULL, at varchar(20), \
         FOREIGN KEY (raven) REFERENCES ravens (id) ON DELETE CASCADE)",
        "CREATE TABLE nests (id int PRIMARY KEY, raven int, FOREIGN KEY (raven) REFERENCES ravens (id) ON DELETE SET NULL)",
        "INSERT INTO owners VALUES ('aesir', 'Odin'), ('aesir', 'Loki')",
        "INSERT INTO ravens VALUES (1, 'aesir', 'Odin'), (2, 'aesir', 'Odin'), (3, 'aesir', 'Loki')",
        "INSERT INTO sightings VALUES (1, 'dawn'), (1, 'dusk'), (2, 'noon'), (3, 'night')",
        "INSERT INTO nests VALUES (1, 1), (2, 3)",
    ]).await;
    let mut set = ChangeSet {
        schema: schema.into(),
        table: "owners".into(),
        key_is_primary: true,
        cascade: false,
        changes: vec![RowChange::Delete { key: vec![cv("clan", Some("aesir")), cv("name", Some("Odin"))] }],
    };
    let impact = s.delete_references(&set).await.unwrap();
    assert_eq!(impact.cascaded_rows, 5);
    let refs = impact.references;
    let summary: Vec<_> = refs.iter().map(|r| (r.table.as_str(), r.parent_table.as_str(), r.depth, r.rows)).collect();
    assert_eq!(summary, [("ravens", "owners", 1, 2), ("nests", "ravens", 2, 1), ("sightings", "ravens", 2, 3)]);
    assert_eq!(refs[0].columns, ["clan", "owner"]);
    assert_eq!((refs[1].on_delete.as_str(), refs[2].on_delete.as_str()), ("SET NULL", "CASCADE"));

    assert!(s.commit_changes(&set).await.is_err());
    set.cascade = true;
    assert_eq!(s.commit_changes(&set).await.unwrap(), 6);
    let left = s
        .run_statements(&["SELECT (SELECT COUNT(*) FROM owners), (SELECT COUNT(*) FROM ravens), (SELECT COUNT(*) FROM sightings), \
             (SELECT COUNT(*) FROM nests WHERE raven IS NULL)".into()])
        .await;
    assert_eq!(left[0].rows[0], [Some("1".into()), Some("1".into()), Some("1".into()), Some("1".into())]);
}

#[tokio::test]
#[ignore]
async fn postgres_end_to_end() {
    let s = Session::open("t".into(), pg()).await.unwrap();
    let info = s.info().await;
    assert!(info.server_version.starts_with("17"), "{info:?}");
    assert_eq!(info.schema, "public");
    exec(&s, &[
        "DROP TABLE IF EXISTS people CASCADE",
        "DROP TABLE IF EXISTS nokey",
        "CREATE TABLE people (id int PRIMARY KEY, name text NOT NULL, note text)",
        "INSERT INTO people VALUES (1,'Ann',NULL),(2,'Bob',NULL),(3,'O''Hara \\ x','q')",
        "CREATE VIEW people_view AS SELECT * FROM people",
        "CREATE TABLE nokey (a int, b jsonb)",
        "INSERT INTO nokey VALUES (1,NULL),(1,NULL)",
    ]).await;
    assert!(s.list_schemas().await.unwrap().contains(&"public".to_string()));
    exercise(&s, "public").await;
    cascade(&s, "public").await;
    features(&s, "public").await;
    read_only(pg()).await;

    let dbs = s.list_databases().await.unwrap();
    assert!(dbs.contains(&"postgres".to_string()));
    exec(&s, &["DROP DATABASE IF EXISTS huginn_other", "CREATE DATABASE huginn_other"]).await;
    let info = s.switch_database("huginn_other").await.unwrap();
    assert_eq!(info.database, "huginn_other");
    let r = s.run_statements(&["SELECT current_database()".into()]).await;
    assert_eq!(r[0].rows[0][0].as_deref(), Some("huginn_other"));
    s.switch_database("postgres").await.unwrap();

    // Cancelling interrupts a long query quickly.
    let sleep = ["SELECT pg_sleep(10)".to_string()];
    let started = Instant::now();
    let (results, _) = tokio::join!(s.run_statements(&sleep), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s.cancel().await.unwrap();
    });
    assert!(results[0].error.as_deref().unwrap_or("").contains("cancel"), "{results:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    s.close().await;
}

#[tokio::test]
#[ignore]
async fn mysql_end_to_end() {
    let s = Session::open("t".into(), mysql()).await.unwrap();
    assert_eq!(s.info().await.schema, "shop");
    exec(&s, &[
        "DROP VIEW IF EXISTS people_view",
        "DROP TABLE IF EXISTS people, nokey",
        "CREATE TABLE people (id int PRIMARY KEY, name varchar(50) NOT NULL, note text)",
        "INSERT INTO people VALUES (1,'Ann',NULL),(2,'Bob',NULL),(3,'O''Hara \\\\ x','q')",
        "CREATE VIEW people_view AS SELECT * FROM people",
        "CREATE TABLE nokey (a int, b json)",
        "INSERT INTO nokey VALUES (1,NULL),(1,NULL)",
    ]).await;
    assert!(s.list_schemas().await.unwrap().contains(&"shop".to_string()));
    exercise(&s, "shop").await;
    cascade(&s, "shop").await;
    features(&s, "shop").await;
    read_only(mysql()).await;

    exec(&s, &["CREATE DATABASE IF NOT EXISTS huginn_other"]).await;
    s.use_schema("huginn_other").await.unwrap();
    let r = s.run_statements(&["SELECT DATABASE()".into()]).await;
    assert_eq!(r[0].rows[0][0].as_deref(), Some("huginn_other"));

    let sleep = ["SELECT SLEEP(10) AS s".to_string()];
    let started = Instant::now();
    let (results, _) = tokio::join!(s.run_statements(&sleep), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s.cancel().await.unwrap();
    });
    // KILL QUERY makes SLEEP() return 1 early rather than raising an error.
    assert!(results[0].error.is_some() || results[0].rows[0][0].as_deref() == Some("1"));
    assert!(started.elapsed() < Duration::from_secs(5));
    s.close().await;
}

#[tokio::test]
#[ignore]
async fn sqlite_end_to_end() {
    let path = std::env::temp_dir().join(format!("huginn-{}.db", uuid::Uuid::new_v4()));
    let cfg = ConnectionConfig {
        file_path: path.to_string_lossy().into(),
        ..base(DriverKind::Sqlite)
    };
    let s = Session::open("t".into(), cfg).await.unwrap();
    assert!(s.info().await.server_version.starts_with("SQLite 3"));
    exec(&s, &[
        "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT)",
        "INSERT INTO people VALUES (1,'Ann',NULL),(2,'Bob',NULL),(3,'O''Hara \\ x','q')",
        "CREATE VIEW people_view AS SELECT * FROM people",
        "CREATE TABLE nokey (a INT, b TEXT)",
        "INSERT INTO nokey VALUES (1,NULL),(1,NULL)",
    ]).await;
    assert_eq!(s.list_schemas().await.unwrap(), ["main"]);
    exercise(&s, "main").await;
    exec(&s, &["PRAGMA foreign_keys = ON"]).await;
    cascade(&s, "main").await;
    features(&s, "main").await;
    read_only(s.config.clone()).await;
    s.close().await;
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
#[ignore]
async fn postgres_over_ssh_tunnel() {
    let known_hosts = std::env::temp_dir().join(format!("huginn-known-{}", uuid::Uuid::new_v4()));
    // SAFETY: tests run single-threaded (--test-threads=1).
    unsafe { std::env::set_var("HUGINN_KNOWN_HOSTS", &known_hosts) };
    let cfg = ConnectionConfig {
        host: "huginn-pg".into(),
        port: 5432,
        ssl_mode: SslMode::Disable,
        ssh: SshConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 52222,
            user: "tunnel".into(),
            auth: SshAuth::Password,
            key_path: String::new(),
        },
        ssh_password: Some("huginn".into()),
        ..pg()
    };
    let s = Session::open("t".into(), cfg.clone()).await.unwrap();
    let r = s.run_statements(&["SELECT inet_server_port()".into()]).await;
    assert_eq!(r[0].rows[0][0].as_deref(), Some("5432"));
    s.close().await;
    assert!(std::fs::read_to_string(&known_hosts).unwrap().contains("[127.0.0.1]:52222"));

    // Second connection reuses the learned key; a wrong password fails cleanly.
    Session::open("t".into(), cfg.clone()).await.unwrap().close().await;
    let bad = ConnectionConfig { ssh_password: Some("nope".into()), ..cfg };
    let e = Session::open("t".into(), bad).await.err().unwrap();
    assert!(format!("{e:#}").contains("authentication failed"), "{e:#}");
    let _ = std::fs::remove_file(known_hosts);
}

// ---- Redis, MongoDB, BigQuery ------------------------------------------

fn rows_of(r: &[QueryResult]) -> Vec<Vec<Option<String>>> {
    r.iter().find(|r| r.has_rows).map(|r| r.rows.clone()).unwrap_or_default()
}

fn req(schema: &str, table: &str) -> FetchRequest {
    FetchRequest {
        schema: schema.into(),
        table: table.into(),
        limit: 100,
        offset: 0,
        filters: vec![],
        raw_where: String::new(),
        sort: None,
        key_order: vec![],
    }
}

#[tokio::test]
#[ignore]
async fn redis_end_to_end() {
    let cfg = ConnectionConfig { port: 56379, ..base(DriverKind::Redis) };
    let s = Session::open("t".into(), cfg).await.unwrap();
    assert!(s.info().await.server_version.starts_with("Redis 7"));
    exec(&s, &[
        "FLUSHALL",
        "SET user:1 Ann",
        "HSET user:2 name Bob age 30",
        "RPUSH queue:jobs a b",
        "SADD tags x y",
        "ZADD scores 1.5 ann 2 bob",
        "SET session:abc token EX 100",
        "SET plain \"two words\"",
    ]).await;

    assert_eq!(s.list_schemas().await.unwrap().len(), 16);
    let tables: Vec<String> = s.list_tables("db0").await.unwrap().into_iter().map(|t| t.name).collect();
    assert_eq!(tables, ["*", "queue:*", "session:*", "user:*"]);
    assert!(s.table_columns("db0", "*").await.unwrap()[0].is_primary_key);

    let users = s.fetch_rows(&req("db0", "user:*")).await.unwrap().rows;
    assert_eq!(users, vec![
        vec![Some("user:1".into()), Some("string".into()), None, Some("Ann".into())],
        vec![Some("user:2".into()), Some("hash".into()), None, Some(r#"{"name":"Bob","age":"30"}"#.into())],
    ]);
    let all = s.fetch_rows(&req("db0", "*")).await.unwrap().rows;
    let by_key = |k: &str| all.iter().find(|r| r[0].as_deref() == Some(k)).unwrap().clone();
    assert_eq!(by_key("scores")[3].as_deref(), Some(r#"{"ann":1.5,"bob":2.0}"#));
    assert_eq!(by_key("queue:jobs")[3].as_deref(), Some(r#"["a","b"]"#));
    assert!(by_key("session:abc")[2].as_deref().unwrap().parse::<i64>().unwrap() > 90);
    assert_eq!(s.count_rows(&req("db0", "*")).await.unwrap(), 7);

    let mut filtered = req("db0", "*");
    filtered.filters = vec![Filter { column: "type".into(), op: "=".into(), value: "hash".into() }];
    assert_eq!(s.fetch_rows(&filtered).await.unwrap().rows.len(), 1);
    filtered.filters = vec![Filter { column: "key".into(), op: "contains".into(), value: "1".into() }];
    assert_eq!(s.count_rows(&filtered).await.unwrap(), 1);
    filtered.filters = vec![Filter { column: "value".into(), op: "=".into(), value: "x".into() }];
    assert!(s.fetch_rows(&filtered).await.is_err());

    let set = ChangeSet {
        schema: "db0".into(),
        table: "*".into(),
        key_is_primary: true,
        cascade: false,
        changes: vec![
            RowChange::Update { key: vec![cv("key", Some("user:2"))], values: vec![cv("value", Some(r#"{"name":"Bobby","age":31}"#))] },
            RowChange::Update { key: vec![cv("key", Some("session:abc"))], values: vec![cv("ttl", None)] },
            RowChange::Update { key: vec![cv("key", Some("plain"))], values: vec![cv("key", Some("plain2"))] },
            RowChange::Insert { values: vec![cv("key", Some("board")), cv("type", Some("zset")), cv("value", Some(r#"{"x":3}"#))] },
            RowChange::Delete { key: vec![cv("key", Some("queue:jobs"))] },
        ],
    };
    let preview = s.preview_changes(&set).await.unwrap();
    assert!(preview.contains(&"HSET user:2 name Bobby age 31".to_string()), "{preview:?}");
    assert_eq!(s.commit_changes(&set).await.unwrap(), 5);
    let r = s.run_statements(&["HGET user:2 name".into(), "TTL session:abc".into(), "GET plain2".into(), "ZSCORE board x".into(), "EXISTS queue:jobs".into()]).await;
    let vals: Vec<Option<String>> = r.iter().map(|r| r.rows[0][0].clone()).collect();
    assert_eq!(vals, [Some("Bobby".into()), Some("-1".into()), Some("two words".into()), Some("3".into()), Some("0".into())]);

    // Console: pairs, NULL replies, SELECT and errors.
    let r = s.run_statements(&["HGETALL user:2".into()]).await;
    assert_eq!(r[0].columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["field", "value"]);
    assert_eq!(rows_of(&s.run_statements(&["GET missing".into()]).await), vec![vec![None]]);
    let r = s.run_statements(&["SELECT 1".into(), "DBSIZE".into(), "SELECT 0".into(), "DBSIZE".into()]).await;
    assert_eq!((r[1].rows[0][0].as_deref(), r[3].rows[0][0].as_deref()), (Some("0"), Some("7")));
    assert!(s.run_statements(&["HGET".into()]).await[0].error.is_some());
    s.close().await;
}

#[tokio::test]
#[ignore]
async fn mongodb_end_to_end() {
    let cfg = ConnectionConfig { port: 57017, user: "root".into(), database: "shop".into(), ssl_mode: SslMode::Disable, ..base(DriverKind::Mongodb) };
    let s = Session::open("t".into(), cfg).await.unwrap();
    let info = s.info().await;
    assert!(info.server_version.starts_with("MongoDB 8"), "{info:?}");
    assert_eq!(info.schema, "shop");
    exec(&s, &[
        "db.people.drop()",
        "db.people_view.drop()",
        r#"db.people.insertMany([
            {_id: 1, name: "Ann", age: 41, tags: ["a"], joined: ISODate("2026-01-02T03:04:05Z")},
            {_id: 2, name: "Bob", age: 29, nick: null},
            {_id: 3, name: "O'Hara", age: 35, address: {city: "Oslo"}}
        ])"#,
        r#"db.runCommand({create: "people_view", viewOn: "people", pipeline: [{$match: {age: {$gt: 30}}}]})"#,
    ]).await;

    assert!(s.list_schemas().await.unwrap().contains(&"shop".to_string()));
    let tables = s.list_tables("shop").await.unwrap();
    assert!(tables.iter().any(|t| t.name == "people" && t.kind == "collection"));
    assert!(tables.iter().any(|t| t.name == "people_view" && t.kind == "view"));
    let cols = s.table_columns("shop", "people").await.unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["_id", "name", "age", "tags", "joined", "nick", "address"]);
    assert!(cols[0].is_primary_key && cols[0].data_type == "int" && cols[2].data_type == "int");

    let mut r = req("shop", "people");
    r.sort = Some(Sort { column: "age".into(), desc: true });
    let page = s.fetch_rows(&r).await.unwrap();
    assert_eq!(page.rows.iter().map(|r| r[1].clone().unwrap()).collect::<Vec<_>>(), ["Ann", "O'Hara", "Bob"]);
    assert_eq!(page.rows[0][4].as_deref(), Some(r#"ISODate("2026-01-02T03:04:05Z")"#));
    assert_eq!(page.rows[0][3].as_deref(), Some(r#"["a"]"#));
    assert_eq!(page.rows[1][6].as_deref(), Some(r#"{"city":"Oslo"}"#));

    r.filters = vec![Filter { column: "age".into(), op: ">".into(), value: "30".into() }];
    assert_eq!(s.count_rows(&r).await.unwrap(), 2);
    r.filters = vec![Filter { column: "name".into(), op: "contains".into(), value: "hAr".into() }];
    assert_eq!(s.fetch_rows(&r).await.unwrap().rows.len(), 1);
    r.filters.clear();
    r.raw_where = "{age: {$lt: 30}}".into();
    assert_eq!(s.count_rows(&r).await.unwrap(), 1);

    let set = ChangeSet {
        schema: "shop".into(),
        table: "people".into(),
        key_is_primary: true,
        cascade: false,
        changes: vec![
            RowChange::Update { key: vec![cv("_id", Some("1"))], values: vec![cv("age", Some("42")), cv("tags", None)] },
            RowChange::Insert { values: vec![cv("_id", Some("4")), cv("name", Some("123")), cv("age", Some("50"))] },
            RowChange::Delete { key: vec![cv("_id", Some("2"))] },
        ],
    };
    let preview = s.preview_changes(&set).await.unwrap();
    assert_eq!(preview[1], r#"db.getCollection("people").updateOne({"_id":1}, {"$set":{"age":42,"tags":null}})"#);
    assert_eq!(s.commit_changes(&set).await.unwrap(), 3);
    // Sampled types keep numbers numeric and "123" a string.
    let check = s.run_statements(&[
        "db.people.countDocuments({age: {$type: 'int'}})".into(),
        r#"db.people.countDocuments({name: "123"})"#.into(),
        "db.people.countDocuments({_id: 2})".into(),
    ]).await;
    let counts: Vec<Option<String>> = check.iter().map(|r| r.rows[0][0].clone()).collect();
    assert_eq!(counts, [Some("3".into()), Some("1".into()), Some("0".into())]);

    // Console calls.
    let r = s.run_statements(&["db.people.find({}, {name: 1}).sort({_id: -1}).limit(2)".into()]).await;
    assert_eq!(rows_of(&r), vec![vec![Some("4".into()), Some("123".into())], vec![Some("3".into()), Some("O'Hara".into())]]);
    let r = s.run_statements(&["db.people.aggregate([{$group: {_id: null, total: {$sum: '$age'}}}])".into()]).await;
    assert_eq!(rows_of(&r)[0][1].as_deref(), Some("127"));
    let r = s.run_statements(&["show collections".into(), "use admin".into(), "db.runCommand({ping: 1})".into()]).await;
    assert!(r[0].rows.iter().any(|row| row[0].as_deref() == Some("people")));
    assert_eq!(r[2].rows[0][0].as_deref(), Some("1"));
    let r = s.run_statements(&["db.people.find({".into()]).await;
    assert!(r[0].error.is_some());
    s.close().await;
}

#[tokio::test]
#[ignore]
async fn bigquery_emulator_end_to_end() {
    let cfg = ConnectionConfig {
        host: "http://127.0.0.1:59050".into(),
        database: "huginn-test".into(),
        password: None,
        ..base(DriverKind::Bigquery)
    };
    let s = Session::open("t".into(), cfg).await.unwrap();
    let info = s.info().await;
    assert!(info.read_only);
    assert_eq!(info.server_version, "BigQuery · huginn-test");
    assert_eq!(s.list_schemas().await.unwrap(), ["analytics", "shop"]);
    assert_eq!(s.list_tables("shop").await.unwrap()[0].name, "orders");
    let cols = s.table_columns("shop", "orders").await.unwrap();
    let types: Vec<&str> = cols.iter().map(|c| c.data_type.as_str()).collect();
    // The emulator drops `mode` (REPEATED/REQUIRED) from table schemas; BigQuery itself reports it.
    assert!(matches!(types[4], "ARRAY<STRING>" | "STRING"), "{types:?}");
    assert_eq!([types[0], types[1], types[2], types[3], types[5], types[6]], ["INTEGER", "STRING", "FLOAT", "BOOLEAN", "RECORD", "TIMESTAMP"]);

    let page = s.fetch_rows(&req("shop", "orders")).await.unwrap();
    assert!(page.sql.starts_with("tabledata.list"));
    assert_eq!(page.rows[0], vec![
        Some("1".into()), Some("Ann".into()), Some("12.5".into()), Some("true".into()),
        Some(r#"["gift","rush"]"#.into()), Some(r#"{"city":"Oslo","zip":150}"#.into()), Some("2026-10-01T10:00:00Z".into()),
    ]);
    assert_eq!(page.rows[2][5], None);
    assert_eq!(s.count_rows(&req("shop", "orders")).await.unwrap(), 3);

    let mut r = req("shop", "orders");
    r.filters = vec![Filter { column: "customer".into(), op: "contains".into(), value: "HAR".into() }];
    let found = s.fetch_rows(&r).await.unwrap();
    assert_eq!(found.rows.len(), 1, "{found:?}");
    assert_eq!(found.rows[0][1].as_deref(), Some("O'Hara"));
    r.filters = vec![Filter { column: "total".into(), op: ">".into(), value: "10".into() }];
    r.sort = Some(Sort { column: "total".into(), desc: true });
    assert_eq!(s.fetch_rows(&r).await.unwrap().rows.iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>(), ["2", "1"]);
    assert_eq!(s.count_rows(&r).await.unwrap(), 2);
    r.filters = vec![Filter { column: "id".into(), op: "=".into(), value: "x".into() }];
    assert!(s.fetch_rows(&r).await.is_err());

    let q = s.run_statements(&["SELECT customer, total * 2 AS doubled FROM `huginn-test.shop.orders` WHERE paid ORDER BY id".into()]).await;
    assert!(q[0].error.is_none(), "{q:?}");
    assert_eq!(rows_of(&q), vec![vec![Some("Ann".into()), Some("25".into())], vec![Some("O'Hara".into()), Some("14.5".into())]]);

    let set = ChangeSet { schema: "shop".into(), table: "orders".into(), key_is_primary: false, cascade: false, changes: vec![] };
    assert!(s.commit_changes(&set).await.is_err());
    s.close().await;
}

#[tokio::test]
#[ignore]
async fn redis_and_mongodb_over_ssh_tunnel() {
    let known_hosts = std::env::temp_dir().join(format!("huginn-known-{}", uuid::Uuid::new_v4()));
    // SAFETY: tests run single-threaded (--test-threads=1).
    unsafe { std::env::set_var("HUGINN_KNOWN_HOSTS", &known_hosts) };
    let ssh = SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: 52222,
        user: "tunnel".into(),
        auth: SshAuth::Password,
        key_path: String::new(),
    };
    let redis = ConnectionConfig {
        host: "huginn-redis".into(),
        port: 6379,
        ssh: ssh.clone(),
        ssh_password: Some("huginn".into()),
        ..base(DriverKind::Redis)
    };
    let s = Session::open("t".into(), redis).await.unwrap();
    assert_eq!(rows_of(&s.run_statements(&["PING".into()]).await), vec![vec![Some("PONG".into())]]);
    s.close().await;

    let mongo = ConnectionConfig {
        host: "huginn-mongo".into(),
        port: 27017,
        user: "root".into(),
        ssl_mode: SslMode::Disable,
        ssh,
        ssh_password: Some("huginn".into()),
        ..base(DriverKind::Mongodb)
    };
    let s = Session::open("t".into(), mongo).await.unwrap();
    assert!(s.info().await.server_version.starts_with("MongoDB"));
    s.close().await;
    let _ = std::fs::remove_file(known_hosts);
}
