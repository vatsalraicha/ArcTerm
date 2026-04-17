//! Tauri command handlers exposed to the frontend.
//!
//! Each handler is a thin wrapper around its service (PtyManager,
//! HistoryStore) so the IPC surface and the underlying logic can evolve
//! independently. Handlers return `Result<T, String>`; the string flows
//! back to JS as a rejected promise.

use tauri::{AppHandle, State};

use crate::history::{Entry, HistoryStore};
use crate::pty::PtyManager;

// -- PTY commands --------------------------------------------------------

#[tauri::command]
pub fn pty_spawn(
    app: AppHandle,
    manager: State<'_, PtyManager>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    manager.spawn(app, cols, rows)
}

#[tauri::command]
pub fn pty_write(
    manager: State<'_, PtyManager>,
    id: String,
    data: String,
) -> Result<(), String> {
    manager.write(&id, &data)
}

#[tauri::command]
pub fn pty_resize(
    manager: State<'_, PtyManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    manager.resize(&id, cols, rows)
}

#[tauri::command]
pub fn pty_kill(manager: State<'_, PtyManager>, id: String) -> Result<(), String> {
    manager.kill(&id)
}

// -- History commands ----------------------------------------------------
//
// All of these take the HistoryStore state. If the store failed to open
// on startup the state isn't registered, and Tauri's error becomes a
// descriptive "state not managed" rejection — better UX than silently
// swallowing the operation.

#[tauri::command]
pub fn history_insert(
    store: State<'_, HistoryStore>,
    command: String,
    cwd: Option<String>,
    started_at: i64,
    session_id: Option<String>,
) -> Result<i64, String> {
    store.insert(&command, cwd.as_deref(), started_at, session_id.as_deref())
}

#[tauri::command]
pub fn history_update_exit(
    store: State<'_, HistoryStore>,
    id: i64,
    exit_code: i64,
    duration_ms: i64,
) -> Result<(), String> {
    store.update_exit(id, exit_code, duration_ms)
}

#[tauri::command]
pub fn history_search(
    store: State<'_, HistoryStore>,
    query: String,
    cwd: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<Entry>, String> {
    store.search(&query, cwd.as_deref(), limit.unwrap_or(50))
}

#[tauri::command]
pub fn history_autosuggest(
    store: State<'_, HistoryStore>,
    prefix: String,
    cwd: Option<String>,
) -> Result<Option<String>, String> {
    store.autosuggest(&prefix, cwd.as_deref())
}
