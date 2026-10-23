mod dialect;
#[cfg(test)]
mod ipc_tests;
#[cfg(test)]
mod live_tests;
mod drivers;
mod model;
mod session;
mod ssh;
mod store;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::RwLock;

use model::*;
use session::Session;
use store::ConnectionStore;

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

struct AppState {
    store: ConnectionStore,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

impl AppState {
    async fn session(&self, id: &str) -> CmdResult<Arc<Session>> {
        self.sessions
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| "This connection is closed. Reconnect to continue.".to_string())
    }
}

// ---- saved connections --------------------------------------------------

#[tauri::command]
async fn list_connections(state: State<'_, AppState>) -> CmdResult<Vec<ConnectionConfig>> {
    state.store.list().map_err(err)
}

#[tauri::command]
async fn save_connection(
    state: State<'_, AppState>,
    config: ConnectionConfig,
) -> CmdResult<ConnectionConfig> {
    state.store.save(config).map_err(err)
}

#[tauri::command]
async fn delete_connection(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.store.delete(&id).map_err(err)
}

#[tauri::command]
async fn test_connection(state: State<'_, AppState>, mut config: ConnectionConfig) -> CmdResult<String> {
    state.store.fill_secrets(&mut config);
    let session = Session::open(String::new(), config).await.map_err(err)?;
    let info = session.info().await;
    session.close().await;
    Ok(info.server_version)
}

// ---- sessions -----------------------------------------------------------

#[tauri::command]
async fn connect(state: State<'_, AppState>, mut config: ConnectionConfig) -> CmdResult<SessionInfo> {
    state.store.fill_secrets(&mut config);
    let id = uuid::Uuid::new_v4().to_string();
    let session = Session::open(id.clone(), config).await.map_err(err)?;
    let info = session.info().await;
    state.sessions.write().await.insert(id, Arc::new(session));
    Ok(info)
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    if let Some(session) = state.sessions.write().await.remove(&session_id) {
        session.close().await;
    }
    Ok(())
}

#[tauri::command]
async fn list_databases(state: State<'_, AppState>, session_id: String) -> CmdResult<Vec<String>> {
    state.session(&session_id).await?.list_databases().await.map_err(err)
}

#[tauri::command]
async fn switch_database(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> CmdResult<SessionInfo> {
    state.session(&session_id).await?.switch_database(&database).await.map_err(err)
}

#[tauri::command]
async fn use_schema(state: State<'_, AppState>, session_id: String, schema: String) -> CmdResult<()> {
    state.session(&session_id).await?.use_schema(&schema).await.map_err(err)
}

#[tauri::command]
async fn list_schemas(state: State<'_, AppState>, session_id: String) -> CmdResult<Vec<String>> {
    state.session(&session_id).await?.list_schemas().await.map_err(err)
}

#[tauri::command]
async fn list_tables(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
) -> CmdResult<Vec<TableInfo>> {
    state.session(&session_id).await?.list_tables(&schema).await.map_err(err)
}

#[tauri::command]
async fn table_columns(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
    table: String,
) -> CmdResult<Vec<ColumnInfo>> {
    state.session(&session_id).await?.table_columns(&schema, &table).await.map_err(err)
}

#[tauri::command]
async fn schema_columns(
    state: State<'_, AppState>,
    session_id: String,
    schema: String,
) -> CmdResult<BTreeMap<String, Vec<String>>> {
    state.session(&session_id).await?.schema_columns(&schema).await.map_err(err)
}

#[tauri::command]
async fn fetch_rows(
    state: State<'_, AppState>,
    session_id: String,
    request: FetchRequest,
) -> CmdResult<QueryResult> {
    state.session(&session_id).await?.fetch_rows(&request).await.map_err(err)
}

#[tauri::command]
async fn count_rows(
    state: State<'_, AppState>,
    session_id: String,
    request: FetchRequest,
) -> CmdResult<u64> {
    state.session(&session_id).await?.count_rows(&request).await.map_err(err)
}

#[tauri::command]
async fn run_query(
    state: State<'_, AppState>,
    session_id: String,
    statements: Vec<String>,
) -> CmdResult<Vec<QueryResult>> {
    Ok(state.session(&session_id).await?.run_statements(&statements).await)
}

#[tauri::command]
async fn cancel_query(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    state.session(&session_id).await?.cancel().await.map_err(err)
}

#[tauri::command]
async fn preview_changes(
    state: State<'_, AppState>,
    session_id: String,
    changes: ChangeSet,
) -> CmdResult<Vec<String>> {
    state.session(&session_id).await?.preview_changes(&changes).await.map_err(err)
}

#[tauri::command]
async fn commit_changes(
    state: State<'_, AppState>,
    session_id: String,
    changes: ChangeSet,
) -> CmdResult<u64> {
    state.session(&session_id).await?.commit_changes(&changes).await.map_err(err)
}

/// The default macOS menu binds ⌘W to "Close Window"; the app uses ⌘W, ⌘T and
/// friends for tabs, so only keep the items that need native handling.
fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let about = AboutMetadata {
        name: Some("HuginnTables".into()),
        comments: Some("A database client for PostgreSQL, MySQL and SQLite".into()),
        ..Default::default()
    };
    Menu::with_items(
        app,
        &[
            &Submenu::with_items(
                app,
                "HuginnTables",
                true,
                &[
                    &PredefinedMenuItem::about(app, None, Some(about))?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::show_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(app, None)?,
                    &PredefinedMenuItem::maximize(app, None)?,
                    &PredefinedMenuItem::fullscreen(app, None)?,
                ],
            )?,
        ],
    )
}

fn with_commands<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .invoke_handler(tauri::generate_handler![
            list_connections,
            save_connection,
            delete_connection,
            test_connection,
            connect,
            disconnect,
            list_databases,
            switch_database,
            use_schema,
            list_schemas,
            list_tables,
            table_columns,
            schema_columns,
            fetch_rows,
            count_rows,
            run_query,
            cancel_query,
            preview_changes,
            commit_changes,
        ])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    with_commands(tauri::Builder::default())
        .plugin(tauri_plugin_dialog::init())
        .menu(app_menu)
        .on_menu_event(|app, event| {
            if event.id() == "settings" {
                let _ = app.emit("open-settings", ());
            }
        })
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            app.manage(AppState {
                store: ConnectionStore::new(dir),
                sessions: RwLock::new(HashMap::new()),
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running HuginnTables");
}
