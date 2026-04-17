//! Library entry point. `main.rs` is a thin shim so the same code can be
//! reused by integration tests and (later) the VS Code extension's host
//! process if we ever decide to share the Rust core.

pub mod ipc;
pub mod pty;
pub mod shell_hooks;

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

    let pty_manager = PtyManager::new(shell_paths);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // PtyManager owns all live PTYs. Stored in Tauri state so command
        // handlers can reach it via `tauri::State<PtyManager>`.
        .manage(pty_manager)
        .invoke_handler(tauri::generate_handler![
            ipc::pty_spawn,
            ipc::pty_write,
            ipc::pty_resize,
            ipc::pty_kill,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ArcTerm");
}
