//! Library entry point. `main.rs` is a thin shim so the same code can be
//! reused by integration tests and (later) the VS Code extension's host
//! process if we ever decide to share the Rust core.

pub mod ai;
pub mod completion;
pub mod history;
pub mod ipc;
pub mod models;
pub mod pty;
pub mod settings;
pub mod shell_hooks;

use std::sync::Arc;

use ai::claude::ClaudeCliBackend;
use ai::local_llama::LocalLlamaBackend;
use ai::router::Mode;
use ai::{AiBackend, AiRouter};
use history::HistoryStore;
use models::downloader::DownloadLock;
use pty::PtyManager;
use settings::SettingsStore;

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

    // Settings: read ~/.arcterm/config.json. Load failures fall back to
    // defaults rather than aborting boot — a broken config shouldn't mean
    // a broken terminal.
    let settings = match SettingsStore::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            log::warn!("settings load failed ({e}), using in-memory defaults");
            // Fall back to a store that still persists to disk on change
            // but starts from defaults. open() only fails on HOME lookup
            // failure, which is fatal for many other things too.
            panic!("fatal: cannot set up settings store: {e}");
        }
    };

    let initial_settings = settings.get();
    let desired_mode = Mode::parse(&initial_settings.ai.mode).unwrap_or(Mode::Auto);

    // Local backend: try to load a GGUF at boot. Three-tier resolution:
    //   1. The exact id pinned in settings.ai.localModel (user's choice).
    //   2. If that isn't installed, any installed registry entry (so
    //      someone who downloaded IQ2_M but still has "q4km" pinned in
    //      their config still gets a working local backend).
    //   3. If nothing is installed, None — Claude mode is still fine.
    //
    // We only pay the load cost when the mode needs local (Local/Auto).
    // Claude-only users get fast boot even with a 3 GB GGUF on disk.
    let local_backend: Option<Arc<LocalLlamaBackend>> =
        if matches!(desired_mode, Mode::Local | Mode::Auto) {
            let pinned = models::find(&initial_settings.ai.local_model)
                .filter(|spec| spec.is_installed());
            let fallback = pinned.or_else(|| {
                models::REGISTRY.iter().find(|s| s.is_installed())
            });
            match fallback {
                Some(spec) => {
                    log::info!(
                        "loading local model at boot: id={} path={}",
                        spec.id,
                        spec.local_path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    );
                    let path = spec.local_path().unwrap();
                    match LocalLlamaBackend::load(path) {
                        Ok(b) => Some(Arc::new(b)),
                        Err(e) => {
                            log::warn!("local model load failed at boot: {e}");
                            None
                        }
                    }
                }
                None => None,
            }
        } else {
            None
        };

    // Claude backend: always registered. is_available() gates its use.
    let claude_backend: Arc<dyn AiBackend> = Arc::new(ClaudeCliBackend::default());

    let ai_router: Arc<AiRouter> = Arc::new(AiRouter::new(
        claude_backend,
        local_backend,
        desired_mode,
    ));

    // Download lock: only one model download in flight at once.
    let download_lock: Arc<DownloadLock> = Arc::new(DownloadLock::default());

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // PtyManager owns all live PTYs. Stored in Tauri state so command
        // handlers can reach it via `tauri::State<PtyManager>`.
        .manage(pty_manager)
        .manage(ai_router)
        .manage(settings)
        .manage(download_lock);

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
            ipc::ai_set_mode,
            ipc::ai_status,
            ipc::settings_get,
            ipc::settings_set,
            ipc::model_list,
            ipc::model_download,
            ipc::model_delete,
            ipc::fs_complete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ArcTerm");
}
