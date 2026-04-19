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

use tauri::menu::{
    AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::Emitter;

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

    // Startup sweep: remove any stranded `.part` files from a previous
    // crashed/killed download. Cheap (one read_dir), idempotent, and
    // prevents gigabyte-scale disk leaks when the user re-tries a fetch.
    let (parts_removed, parts_bytes) = models::cleanup_stranded_parts();
    if parts_removed > 0 {
        log::info!(
            "cleaned up {parts_removed} stranded download file(s), reclaimed {} MB",
            parts_bytes / 1_048_576
        );
    }

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
    // SECURITY FIX: don't panic the whole app because settings couldn't open.
    // Previously a corrupt / unreadable config.json would crash boot. Fall
    // back to an ephemeral default store so the terminal still works (user
    // just sees default theme/mode).
    let settings = match SettingsStore::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            log::error!("settings load failed ({e}); using ephemeral defaults");
            Arc::new(SettingsStore::ephemeral())
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
    // SECURITY FIX: honor settings.ai.claudePath on boot. Previously the
    // field was persisted but ignored — users believed they had pinned a
    // path but PATH lookup still won.
    //
    // SECURITY FIX (claudePath validation): validate the persisted path
    // before loading it into the live backend. A config.json that was
    // edited by an attacker (or an older ArcTerm that had no validator)
    // could pin `ai.claudePath` to an arbitrary binary; without this
    // guard, the very first `ai_is_available` probe on boot would spawn
    // it. On violation, log and fall back to PATH lookup — don't refuse
    // to boot, the terminal still works without AI.
    let claude_path_at_boot = match settings::validate_claude_path(
        &initial_settings.ai.claude_path,
    ) {
        Ok(()) => initial_settings.ai.claude_path.as_str(),
        Err(e) => {
            log::warn!(
                "rejecting persisted claudePath ('{}'): {}. Falling back to PATH lookup.",
                initial_settings.ai.claude_path,
                e
            );
            // Scrub the poisoned field from the in-memory store too, so
            // the settings panel doesn't keep re-displaying it as saved.
            let _ = settings.update(|s| s.ai.claude_path.clear());
            ""
        }
    };
    let claude_concrete = Arc::new(ClaudeCliBackend::with_binary(
        claude_path_at_boot,
    ));
    let claude_backend: Arc<dyn AiBackend> = claude_concrete.clone();

    let ai_router: Arc<AiRouter> = Arc::new(AiRouter::new(
        claude_backend,
        local_backend,
        desired_mode,
    ));

    // Download lock: only one model download in flight at once.
    let download_lock: Arc<DownloadLock> = Arc::new(DownloadLock::default());

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // Native macOS menu bar. On macOS, setting a menu here replaces
        // Tauri's default, so we have to recreate the standard items
        // (About, Services, Hide/Show, Quit, Edit verbs, Window). The
        // payoff is our custom "Settings…" entry — ⌘, already opens the
        // settings panel from the keyboard, but users expect to find it
        // under the app menu too.
        .setup(|app| {
            let handle = app.handle();
            let about_meta = AboutMetadataBuilder::new()
                .name(Some("ArcTerm"))
                .version(Some(env!("CARGO_PKG_VERSION")))
                .short_version(Some(env!("CARGO_PKG_VERSION")))
                .build();

            let settings_item = MenuItemBuilder::with_id("arcterm:settings", "Settings…")
                .accelerator("Cmd+,")
                .build(handle)?;

            let app_menu = SubmenuBuilder::new(handle, "ArcTerm")
                .item(&PredefinedMenuItem::about(
                    handle,
                    Some("About ArcTerm"),
                    Some(about_meta),
                )?)
                .separator()
                .item(&settings_item)
                .separator()
                .item(&PredefinedMenuItem::services(handle, None)?)
                .separator()
                .item(&PredefinedMenuItem::hide(handle, None)?)
                .item(&PredefinedMenuItem::hide_others(handle, None)?)
                .item(&PredefinedMenuItem::show_all(handle, None)?)
                .separator()
                .item(&PredefinedMenuItem::quit(handle, None)?)
                .build()?;

            // Edit menu — required for standard ⌘Z/⌘X/⌘C/⌘V to appear as
            // menu entries. WebKit handles these inside the editor
            // without the menu too, but having them visible is a macOS
            // convention users expect.
            let edit_menu = SubmenuBuilder::new(handle, "Edit")
                .item(&PredefinedMenuItem::undo(handle, None)?)
                .item(&PredefinedMenuItem::redo(handle, None)?)
                .separator()
                .item(&PredefinedMenuItem::cut(handle, None)?)
                .item(&PredefinedMenuItem::copy(handle, None)?)
                .item(&PredefinedMenuItem::paste(handle, None)?)
                .item(&PredefinedMenuItem::select_all(handle, None)?)
                .build()?;

            // Window menu — minimize/zoom/close wired to the active window.
            let window_menu = SubmenuBuilder::new(handle, "Window")
                .item(&PredefinedMenuItem::minimize(handle, None)?)
                .item(&PredefinedMenuItem::maximize(handle, None)?)
                .separator()
                .item(&PredefinedMenuItem::close_window(handle, None)?)
                .build()?;

            let menu = MenuBuilder::new(handle)
                .item(&app_menu)
                .item(&edit_menu)
                .item(&window_menu)
                .build()?;
            app.set_menu(menu)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            // One case today. As we add more menu items, route here by id
            // (e.g. `arcterm:new-session` → emit `menu://new-session`).
            if event.id() == "arcterm:settings" {
                let _ = app.emit("menu://settings", ());
            }
        })
        // PtyManager owns all live PTYs. Stored in Tauri state so command
        // handlers can reach it via `tauri::State<PtyManager>`.
        .manage(pty_manager)
        .manage(ai_router)
        .manage(settings)
        .manage(download_lock)
        .manage(claude_concrete);

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
            ipc::ai_set_local_model,
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
