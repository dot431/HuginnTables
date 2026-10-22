//! Drives the real command handlers through Tauri's IPC layer with the same
//! JSON the React UI sends, so argument names and enum tags stay in sync.

use std::collections::HashMap;

use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewWindow};
use tokio::sync::RwLock;

use crate::store::ConnectionStore;
use crate::{with_commands, AppState};

fn call(webview: &WebviewWindow<tauri::test::MockRuntime>, cmd: &str, body: Value) -> Result<Value, Value> {
    get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|b| b.deserialize::<Value>().unwrap())
}

#[test]
fn ui_payloads_round_trip_through_ipc() {
    let dir = std::env::temp_dir().join(format!("huginn-ipc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let app = with_commands(mock_builder()).build(mock_context(noop_assets())).unwrap();
    app.manage(AppState { store: ConnectionStore::new(dir.clone()), sessions: RwLock::new(HashMap::new()) });
    let w = tauri::WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();

    assert_eq!(call(&w, "list_connections", json!({})).unwrap(), json!([]));

    // Shape produced by emptyConnection("sqlite") in src/lib/api.ts.
    let db = dir.join("ipc.sqlite");
    let config = json!({
        "id": "", "name": "ipc", "driver": "sqlite", "color": "#5b8def", "host": "", "port": 0,
        "user": "", "database": "", "sslMode": "prefer", "filePath": db.to_string_lossy(),
        "ssh": { "enabled": false, "host": "", "port": 22, "user": "", "auth": "privateKey", "keyPath": "~/.ssh/id_ed25519" }
    });
    let info = call(&w, "connect", json!({ "config": config })).unwrap();
    assert_eq!(info["driver"], "sqlite");
    assert_eq!(info["schema"], "main");
    let sid = info["sessionId"].as_str().unwrap().to_string();

    let out = call(&w, "run_query", json!({ "sessionId": sid, "statements": [
        "CREATE TABLE pets (id INTEGER PRIMARY KEY, name TEXT NOT NULL, owner TEXT)",
        "INSERT INTO pets (name, owner) VALUES ('Huginn', 'Odin'), ('Muninn', 'Odin'), ('Fenrir', NULL)",
    ]}))
    .unwrap();
    assert_eq!(out[1]["rowsAffected"], 3);
    assert_eq!(out[1]["hasRows"], false);

    assert_eq!(
        call(&w, "list_tables", json!({ "sessionId": sid, "schema": "main" })).unwrap(),
        json!([{ "name": "pets", "kind": "table" }])
    );
    let cols = call(&w, "table_columns", json!({ "sessionId": sid, "schema": "main", "table": "pets" })).unwrap();
    assert_eq!(cols[0]["isPrimaryKey"], true);
    assert_eq!(cols[1]["nullable"], false);

    let request = json!({
        "schema": "main", "table": "pets", "limit": 300, "offset": 0,
        "filters": [{ "column": "name", "op": "contains", "value": "inn" }],
        "rawWhere": "", "sort": { "column": "id", "desc": true }
    });
    let page = call(&w, "fetch_rows", json!({ "sessionId": sid, "request": request })).unwrap();
    assert_eq!(page["rows"], json!([["2", "Muninn", "Odin"], ["1", "Huginn", "Odin"]]));
    assert_eq!(call(&w, "count_rows", json!({ "sessionId": sid, "request": request })).unwrap(), 2);

    let changes = json!({
        "schema": "main", "table": "pets", "keyIsPrimary": true,
        "changes": [
            { "kind": "update", "key": [{ "column": "id", "value": "1" }], "values": [{ "column": "owner", "value": null }] },
            { "kind": "delete", "key": [{ "column": "id", "value": "3" }] },
            { "kind": "insert", "values": [{ "column": "name", "value": "Sleipnir" }] }
        ]
    });
    let sql = call(&w, "preview_changes", json!({ "sessionId": sid, "changes": changes })).unwrap();
    assert_eq!(sql[0], "UPDATE \"main\".\"pets\" SET \"owner\" = NULL WHERE \"id\" = '1'");
    assert_eq!(call(&w, "commit_changes", json!({ "sessionId": sid, "changes": changes })).unwrap(), 3);

    let all = call(&w, "run_query", json!({ "sessionId": sid, "statements": ["SELECT name, owner FROM pets ORDER BY id"] })).unwrap();
    assert_eq!(all[0]["rows"], json!([["Huginn", null], ["Muninn", "Odin"], ["Sleipnir", null]]));
    assert_eq!(all[0]["columns"][0]["name"], "name");

    let err = call(&w, "run_query", json!({ "sessionId": sid, "statements": ["SELECT * FROM nope"] })).unwrap();
    assert!(err[0]["error"].as_str().unwrap().contains("no such table"));

    call(&w, "disconnect", json!({ "sessionId": sid })).unwrap();
    let closed = call(&w, "list_schemas", json!({ "sessionId": sid })).unwrap_err();
    assert!(closed.as_str().unwrap().contains("closed"));
    let _ = std::fs::remove_dir_all(dir);
}
