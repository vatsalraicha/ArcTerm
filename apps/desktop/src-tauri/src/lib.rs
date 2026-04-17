//! Library entry point. `main.rs` is a thin shim so the same code can be
//! reused by integration tests and (later) the VS Code extension's host
//! process if we ever decide to share the Rust core.

pub mod ai;
pub mod completion;
pub mod history;
pub mod ipc;
pub mod pty;
pub mod shell_hooks;

use std::sync::Arc;

use ai::{AiRouter, claude::ClaudeCliBackend};
use history::HistoryStore;
use pty::PtyManager;

/// Build and run the Tauri app. Called from `main.rs`.
pub fn run() {
    // Install shell integration before anyone can spawn a PTY. If this fails
    // we log and continue — the terminal still works without the hooks, the
    // user just sees their normal shell prompt alongside ours.
    let shell_paths = match shell_hooks::install() {
        Ok(p) => Some(p),
        Err(e) => {
            log::warn!("shell integration install failed: {e}");
            None
        }
    };

    // History is optional in the same sense — if SQLite can't open we log
    // and continue without autosuggest/overlay features.
    let history = match HistoryStore::open() {
        Ok(h) => Some(h),
        Err(e) => {
            log::warn!("history store unavailable: {e}");
            None
        }
    };

    let pty_manager = PtyManager::new(shell_paths);

    // AI router. Phase 5a only registers Claude CLI; the router pattern is
    // here so Phase 5b can add LocalLlamaBackend and an auto-fallback layer
    // without changing command handlers or frontend code.
    let ai_router: Arc<AiRouter> = Arc::new(AiRouter::new(Arc::new(
        ClaudeCliBackend::default(),
    )));

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // PtyManager owns all live PTYs. Stored in Tauri state so command
        // handlers can reach it via `tauri::State<PtyManager>`.
        .manage(pty_manager)
        .manage(ai_router);

    // Only register the HistoryStore state when the DB opened cleanly.
    // Absent state makes history_* commands fail with a clear error rather
    // than silently misbehaving.
    if let Some(h) = history {
        builder = builder.manage(h);
    }

    builder
        .invoke_handler(tauri::generate_handler![
            ipc::pty_spawn,
            ipc::pty_write,
            ipc::pty_resize,
            ipc::pty_kill,
            ipc::history_insert,
            ipc::history_update_exit,
            ipc::history_search,
            ipc::history_autosuggest,
            ipc::ai_is_available,
            ipc::ai_active_backend,
            ipc::ai_ask,
            ipc::ai_stream,
            ipc::fs_complete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ArcTerm");
}
