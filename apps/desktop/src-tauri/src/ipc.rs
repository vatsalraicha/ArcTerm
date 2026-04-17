//! Tauri command handlers exposed to the frontend.
//!
//! Each handler is a thin wrapper around `PtyManager` so the IPC surface and
//! the underlying PTY logic can evolve independently. Handlers return
//! `Result<T, String>`; the string flows back to JS as a rejected promise.

use tauri::{AppHandle, State};

use crate::pty::PtyManager;

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
