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
        host: "127.0.0.1".into(),
        port: 0,
        user: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Prefer,
        file_path: String::new(),
        ssh: SshConfig::default(),
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
