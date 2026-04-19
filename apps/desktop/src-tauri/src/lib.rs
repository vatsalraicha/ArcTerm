//! Library entry point. `main.rs` is a thin shim so the same code can be
//! reused by integration tests and (later) the VS Code extension's host
//! process if we ever decide to share the Rust core.

pub mod ai;
pub mod completion;
pub mod history;
pub mod ipc;
pub mod ipc_guard;
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
use ipc_guard::AuditLog;
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

    // Wave 4: normalize GGUF file permissions to 0600. Fresh downloads
    // already get 0600 at atomic-rename time (Wave 2), but files
    // installed earlier kept umask-default perms. Sweeps once per boot;
    // no-op on files already at 0600 so cheap to run unconditionally.
    let (perms_normalized, perms_total) = models::normalize_model_perms();
    if perms_normalized > 0 {
        log::info!(
            "normalized perms on {perms_normalized}/{perms_total} model file(s) to 0600"
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
    // Wave 2.5: NEVER load the local model on the boot critical path.
    //
    // Previously we eagerly loaded the GGUF here (synchronously,
    // in-process) so the router was fully wired before `.run()` opened
    // the window. Two problems:
    //
    //   1. UX: a 3–8 GB GGUF mmap + Metal shader compile takes 3–10 s
    //      of wall time. The Dock icon appears immediately but the
    //      window doesn't render until this finishes.
    //   2. Security: to close the GGUF-parser-CVE exposure (see the
    //      llama.cpp advisory sweep — 6 of 6 applicable CVEs in 2025-26
    //      fire at load, not inference) we now call `load_verified`
    //      everywhere, which SHA-256s the full file before the parser
    //      sees it. That adds another 15–40 s on top of the mmap. Put
    //      together, eager boot-load would be a minute of black window.
    //
    // Instead: router boots with `local = None`. A setup-hook task
    // (registered further down) spawns the verify+load in the
    // background AFTER the window appears; the router transitions
    // from "Claude only" to "Claude + Local / Auto" mid-session via
    // `install_local`. The toolbar pill shows the state live.
    //
    // Claude-mode users pay zero — the setup-hook early-exits when
    // desired_mode is Claude.
    let local_backend: Option<Arc<LocalLlamaBackend>> = None;

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

    // Wave 5 (lite): in-memory audit log of sensitive IPC calls.
    // Forensic trail for post-incident investigation — not a defense.
    // See ipc_guard module docs for the scope rationale.
    let audit_log: Arc<AuditLog> = Arc::new(AuditLog::new());

    // Wave 2.5: clones captured into the setup closure for the background
    // local-model load. The router + settings are also registered via
    // `.manage()` further down for IPC handlers; the clones here are a
    // separate Arc-borrow so we don't depend on `app.state::<>()` being
    // populated at setup-hook firing time.
    let ai_router_boot = ai_router.clone();
    let settings_boot = settings.clone();

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // Native macOS menu bar. On macOS, setting a menu here replaces
        // Tauri's default, so we have to recreate the standard items
        // (About, Services, Hide/Show, Quit, Edit verbs, Window). The
        // payoff is our custom "Settings…" entry — ⌘, already opens the
        // settings panel from the keyboard, but users expect to find it
        // under the app menu too.
        .setup(move |app| {
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

            // Wave 2.5: fire off the background local-model load now that
            // the window is about to render. Gated on the persisted mode —
            // Claude-only users pay zero. The task does:
            //   1. Resolve a pinned-or-fallback installed spec.
            //   2. Emit `ai://local-loading` so the toolbar pill renders.
            //   3. SHA-verify + mmap + Metal compile (15–40 s total).
            //   4. On success: `install_local`, emit `ai://local-ready`.
            //   5. On failure: emit `ai://local-load-failed` with the error.
            if matches!(desired_mode, Mode::Local | Mode::Auto) {
                spawn_local_load(
                    app.handle().clone(),
                    ai_router_boot.clone(),
                    settings_boot.clone(),
                    desired_mode,
                );
            }
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
        .manage(claude_concrete)
        .manage(audit_log);

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
            ipc::ipc_audit_tail,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ArcTerm");
}

/// Spawn the Wave 2.5 background local-model load.
///
/// Fires after the setup hook (i.e. after the main window has been created
/// and the user can already type in the terminal). Picks the pinned spec
/// with a fallback to any installed registry entry, marks the router as
/// "loading", runs `load_verified` on a blocking thread, and either swaps
/// the loaded backend into the router or reports a failure to the
/// frontend via a Tauri event.
///
/// Event wire format (consumed by `apps/desktop/src/main.ts`):
///   `ai://local-loading`     → `{id, display_name, quantization}`
///   `ai://local-ready`       → `{id, display_name, quantization}`
///   `ai://local-load-failed` → `{id, error}`
///
/// No-ops silently if no registry entry is installed. That case flows
/// through naturally — `ai_is_available` returns whatever Claude says,
/// the toolbar pill stays hidden (nothing to show), and AI features
/// degrade to Claude-only without surfacing a noisy error.
fn spawn_local_load(
    app: tauri::AppHandle,
    router: Arc<ai::AiRouter>,
    settings: Arc<SettingsStore>,
    desired_mode: Mode,
) {
    // Snapshot the settings once for this boot. `verify_on_boot` is the
    // user-facing escape hatch for power users on slow disks; default
    // true (secure). We read it here and pass it into the blocking task
    // so the toolbar pill still emits a "loading" event regardless, but
    // the verify step itself becomes a no-op when disabled.
    let settings_snapshot = settings.get();
    let verify_on_boot = settings_snapshot.ai.verify_on_boot;
    // Resolve which spec we're going to try. Same three-tier logic as
    // the previous boot-sync path: pinned > any installed > nothing.
    let pinned_id = settings_snapshot.ai.local_model;
    let spec = match models::find(&pinned_id).filter(|s| s.is_installed()) {
        Some(s) => s,
        None => match models::REGISTRY.iter().find(|s| s.is_installed()) {
            Some(s) => s,
            None => {
                log::info!(
                    "no local model installed; skipping background load \
                     (mode={} will fall back to Claude)",
                    desired_mode.as_str()
                );
                return;
            }
        },
    };

    let path = match spec.local_path() {
        Some(p) => p,
        None => {
            log::warn!("local_path unavailable for {}; aborting bg load", spec.id);
            return;
        }
    };

    // Populate the router's loading slot so `ai_status` can report it
    // immediately — a frontend reload (⌘R) mid-load would otherwise miss
    // the loading-event emission that happens below.
    let info = ai::router::LoadingInfo {
        id: spec.id.to_string(),
        display_name: spec.display_name.to_string(),
        quantization: spec.quantization.to_string(),
    };
    router.set_loading(Some(info.clone()));

    // Emit `ai://local-loading` for live subscribers. Safe to ignore the
    // Result — a dropped channel just means no frontend is listening.
    let loading_payload = serde_json::json!({
        "id": info.id,
        "display_name": info.display_name,
        "quantization": info.quantization,
    });
    let _ = app.emit("ai://local-loading", loading_payload.clone());

    // Run the actual load on Tauri's blocking pool. We use
    // `tauri::async_runtime::spawn_blocking` rather than
    // `tokio::task::spawn_blocking` because Tauri 2 wraps tokio in its
    // own runtime abstraction; calling `tokio::task::spawn_blocking`
    // from inside a `tauri::async_runtime::spawn` closure has been
    // observed to wedge at 0 % CPU on boot (the nested tokio API
    // doesn't reach Tauri's blocking-thread pool and the task never
    // gets scheduled). Checkpoint logs at each boundary make it easy
    // to tell whether a future hang is in lock acquisition, SHA
    // verify, llama.cpp load, or install_local.
    //
    // We hold `lock_loads` across the whole verify+load+install
    // sequence so a user-triggered `ai_set_local_model` or
    // `model_delete` racing with us can't corrupt shared state.
    let log_spec_id = spec.id.to_string();
    tauri::async_runtime::spawn(async move {
        log::info!("[bg-load {log_spec_id}] task started; acquiring load_lock");
        let _guard = router.lock_loads().await;
        log::info!("[bg-load {log_spec_id}] load_lock acquired; spawning blocking");

        let path_for_blocking = path.clone();
        let app_for_progress = app.clone();
        let spec_id = log_spec_id.clone();
        let load_result = tauri::async_runtime::spawn_blocking(move || {
            if verify_on_boot {
                log::info!("[bg-load {spec_id}] blocking task start; beginning SHA verify");
                let mut last_pct: i32 = -1;
                let spec_id_for_cb = spec_id.clone();
                let app_for_cb = app_for_progress.clone();
                let verify_result = crate::models::verify_integrity_with_progress(
                    &path_for_blocking,
                    |bytes, total| {
                        if total == 0 {
                            return;
                        }
                        let pct = ((bytes * 100) / total) as i32;
                        if pct != last_pct {
                            last_pct = pct;
                            let _ = app_for_cb.emit(
                                "ai://local-loading-progress",
                                serde_json::json!({
                                    "id": spec_id_for_cb,
                                    "phase": "verify",
                                    "percent": pct,
                                }),
                            );
                        }
                    },
                );
                verify_result?;
                log::info!("[bg-load {spec_id}] SHA verify complete; calling llama-cpp-2 load");
            } else {
                log::warn!(
                    "[bg-load {spec_id}] verify_on_boot=false; SKIPPING SHA re-verify. \
                     Trusting on-disk bytes. All user-initiated swap paths still verify."
                );
                // Push a single 100% "verify" progress event so the pill's
                // state machine transitions straight to the compile phase
                // without sitting confused on "0%".
                let _ = app_for_progress.emit(
                    "ai://local-loading-progress",
                    serde_json::json!({
                        "id": spec_id,
                        "phase": "verify",
                        "percent": 100,
                    }),
                );
            }
            let _ = app_for_progress.emit(
                "ai://local-loading-progress",
                serde_json::json!({
                    "id": spec_id,
                    "phase": "compiling",
                    "percent": 100,
                }),
            );
            let r = ai::local_llama::LocalLlamaBackend::load(path_for_blocking);
            log::info!(
                "[bg-load {spec_id}] llama-cpp-2 load returned: {}",
                if r.is_ok() { "ok" } else { "err" }
            );
            r
        })
        .await;
        log::info!("[bg-load {log_spec_id}] blocking task joined");

        match load_result {
            Ok(Ok(backend)) => {
                let switch_to_auto = false;
                if let Err(e) = router.install_local(Arc::new(backend), switch_to_auto) {
                    log::warn!("install_local after bg load failed: {e}");
                    router.set_loading(None);
                    let _ = app.emit(
                        "ai://local-load-failed",
                        serde_json::json!({ "id": spec.id, "error": e }),
                    );
                    return;
                }
                // Restore the user's persisted mode preference. At
                // construction time the router clamped Local/Auto to
                // Claude because local was None; now that local is
                // loaded, re-apply what the user actually configured.
                // Without this the active backend stays Claude even
                // when the user expected Local.
                if let Err(e) = router.set_mode(desired_mode) {
                    log::warn!(
                        "restoring mode to {} after bg load failed: {e}",
                        desired_mode.as_str()
                    );
                }
                router.set_loading(None);
                log::info!("background local-model load complete: {}", spec.id);
                let _ = app.emit("ai://local-ready", loading_payload);
            }
            Ok(Err(e)) => {
                log::warn!("background local-model load failed: {e}");
                router.set_loading(None);
                let _ = app.emit(
                    "ai://local-load-failed",
                    serde_json::json!({ "id": spec.id, "error": e }),
                );
            }
            Err(join_err) => {
                let msg = format!("load task panicked: {join_err}");
                log::warn!("{msg}");
                router.set_loading(None);
                let _ = app.emit(
                    "ai://local-load-failed",
                    serde_json::json!({ "id": spec.id, "error": msg }),
                );
            }
        }
    });
}
