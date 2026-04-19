//! Tauri command handlers exposed to the frontend.
//!
//! Each handler is a thin wrapper around its service (PtyManager,
//! HistoryStore) so the IPC surface and the underlying logic can evolve
//! independently. Handlers return `Result<T, String>`; the string flows
//! back to JS as a rejected promise.

use std::sync::Arc;

use futures::StreamExt;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use std::path::PathBuf;

use crate::ai::router::Mode;
use crate::ai::{AiChunk, AiContext, AiRequest, AiResponse, AiRouter};
use crate::completion::{complete as fs_complete_impl, CompletionResult};
use crate::history::{Entry, HistoryStore};
use crate::models::{self, downloader, ModelInfo};
use crate::pty::PtyManager;
use crate::settings::{Settings, SettingsStore};

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

/// SECURITY FIX: hard cap on a single pty_write payload. A compromised or
/// buggy renderer could otherwise shove arbitrarily large strings at the
/// shell. 1 MiB is orders of magnitude larger than any legitimate keystroke
/// burst or paste (the shell itself has READLINE_LINE limits well below
/// this) but bounded enough to prevent trivial memory pressure attacks.
const PTY_WRITE_MAX_BYTES: usize = 1 << 20;

#[tauri::command]
pub fn pty_write(
    manager: State<'_, PtyManager>,
    id: String,
    data: String,
) -> Result<(), String> {
    if data.len() > PTY_WRITE_MAX_BYTES {
        return Err(format!(
            "pty_write payload too large ({} bytes; max {})",
            data.len(),
            PTY_WRITE_MAX_BYTES
        ));
    }
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

// -- AI commands ---------------------------------------------------------
//
// The router state is registered optionally (see lib.rs). When the user has
// no backend configured at all, these commands fail with a clear error
// which the frontend translates into "AI features unavailable".

/// Cheap availability check. Backs the frontend's decision to show or hide
/// the Cmd+K panel / explain buttons. Called during boot and cached.
#[tauri::command]
pub async fn ai_is_available(router: State<'_, Arc<AiRouter>>) -> Result<bool, String> {
    Ok(router.inner().is_available().await)
}

/// Reveal the active backend's id + display name. Frontend uses this to
/// label responses ("via Claude") and to pick appropriate UI affordances.
#[tauri::command]
pub fn ai_active_backend(
    router: State<'_, Arc<AiRouter>>,
) -> Result<serde_json::Value, String> {
    let b = router.inner().active();
    Ok(serde_json::json!({
        "id": b.id(),
        "display_name": b.display_name(),
    }))
}

/// Single-shot ask. Returns the full response when complete. Used by the
/// Cmd+K "generate command" flow where we need the whole answer before
/// populating the editor.
#[tauri::command]
pub async fn ai_ask(
    router: State<'_, Arc<AiRouter>>,
    history: State<'_, HistoryStore>,
    req: AiRequest,
) -> Result<AiResponse, String> {
    let enriched = AiRequest {
        context: req.context.map(|c| crate::ai::context::enrich(c, Some(history.inner()))),
        ..req
    };
    router.inner().ask(enriched).await
}

/// Streaming ask. Returns a request id synchronously; chunks arrive on the
/// `ai://chunk` event with matching `id`. The caller listens until a chunk
/// with `done: true` lands.
#[tauri::command]
pub async fn ai_stream(
    app: AppHandle,
    router: State<'_, Arc<AiRouter>>,
    history: State<'_, HistoryStore>,
    req: AiRequest,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();
    let enriched = AiRequest {
        context: req.context.map(|c| crate::ai::context::enrich(c, Some(history.inner()))),
        ..req
    };
    let backend = router.inner().active();
    let stream_id = id.clone();

    // Spawn on the tokio runtime: the backend's stream is async but the
    // Tauri command handler returns immediately with the id.
    tokio::spawn(async move {
        let mut stream = backend.stream(enriched);
        while let Some(item) = stream.next().await {
            match item {
                Ok(delta) => {
                    let chunk = AiChunk {
                        id: stream_id.clone(),
                        delta,
                        done: false,
                        error: None,
                    };
                    if let Err(e) = app.emit("ai://chunk", chunk) {
                        log::warn!("ai://chunk emit failed: {e}");
                        return;
                    }
                }
                Err(err) => {
                    let _ = app.emit(
                        "ai://chunk",
                        AiChunk {
                            id: stream_id.clone(),
                            delta: String::new(),
                            done: true,
                            error: Some(err),
                        },
                    );
                    return;
                }
            }
        }
        // Clean end of stream.
        let _ = app.emit(
            "ai://chunk",
            AiChunk {
                id: stream_id,
                delta: String::new(),
                done: true,
                error: None,
            },
        );
    });

    Ok(id)
}

// Provide a way to construct an empty context from the frontend when the
// caller only wants to send a raw question. Not a command — used inside
// the ai_ask path as a default.
#[allow(dead_code)]
pub fn empty_context() -> AiContext {
    AiContext::default()
}

// -- Settings -----------------------------------------------------------

#[tauri::command]
pub fn settings_get(store: State<'_, std::sync::Arc<SettingsStore>>) -> Settings {
    store.inner().get()
}

#[tauri::command]
pub fn settings_set(
    store: State<'_, std::sync::Arc<SettingsStore>>,
    claude: State<'_, std::sync::Arc<crate::ai::claude::ClaudeCliBackend>>,
    settings: Settings,
) -> Result<(), String> {
    // SECURITY FIX: apply the claudePath change to the live backend. Without
    // this, persisting a new path had no effect until process restart.
    claude.inner().set_binary(&settings.ai.claude_path);
    store.inner().set(settings)
}

// -- Models -------------------------------------------------------------

#[tauri::command]
pub fn model_list() -> Vec<ModelInfo> {
    models::list()
}

#[tauri::command]
pub async fn model_download(
    app: AppHandle,
    router: State<'_, std::sync::Arc<AiRouter>>,
    settings: State<'_, std::sync::Arc<SettingsStore>>,
    download_lock: State<'_, std::sync::Arc<downloader::DownloadLock>>,
    id: String,
) -> Result<(), String> {
    let spec = models::find(&id)
        .ok_or_else(|| format!("unknown model id: {id}"))?;
    let _guard = download_lock.inner().try_acquire(&id)?;

    let result = downloader::download(app.clone(), spec).await;
    let done = match &result {
        Ok(path) => downloader::DonePayload {
            id: id.clone(),
            success: true,
            error: None,
            local_path: Some(path.clone()),
        },
        Err(err) => downloader::DonePayload {
            id: id.clone(),
            success: false,
            error: Some(err.clone()),
            local_path: None,
        },
    };

    // On success, load the model into memory and hand it to the router.
    // Loading a 3 GB GGUF + compiling Metal shaders is slow, so we do
    // it on a blocking thread to keep the Tauri async runtime free.
    if let Ok(path) = &result {
        let path = PathBuf::from(path);
        let router_arc = router.inner().clone();
        let load_result = tokio::task::spawn_blocking(move || {
            crate::ai::local_llama::LocalLlamaBackend::load(path)
        })
        .await
        .map_err(|e| format!("load join: {e}"))?;

        match load_result {
            Ok(backend) => {
                // Switch mode to Auto so the newly-available local model
                // actually gets used in the absence of Claude.
                let _ = router_arc.install_local(std::sync::Arc::new(backend), true);
                // Persist the model id AND the mode switch to auto so the
                // next app boot reloads THIS variant and actually uses it.
                // Without the mode persist, lib.rs's Claude-mode fast-boot
                // skips local loading entirely and /arcterm-model local
                // errors after restart. The in-memory install_local above
                // already flipped the runtime mode — we just mirror it to
                // disk here.
                let id_for_persist = id.clone();
                let _ = settings.inner().update(move |s| {
                    s.ai.local_model = id_for_persist;
                    s.ai.mode = "auto".to_string();
                });
            }
            Err(e) => {
                log::warn!("model downloaded but load failed: {e}");
            }
        }
    }

    let _ = app.emit(downloader::EVENT_DONE, done);
    result.map(|_| ())
}

#[tauri::command]
pub async fn model_delete(
    router: State<'_, std::sync::Arc<AiRouter>>,
    id: String,
) -> Result<(), String> {
    let spec = models::find(&id)
        .ok_or_else(|| format!("unknown model id: {id}"))?;
    downloader::uninstall(spec).await?;
    // Drop our in-memory copy of this backend if it was the active one.
    // Today we only ship one local model slot, so uninstall always
    // empties the local backend. Multi-model support (Phase 7) will
    // need to check id equality.
    router.inner().uninstall_local();
    Ok(())
}

// -- Router mode switching ---------------------------------------------

/// Load a specific local model into the router, replacing any previously-
/// loaded one. Idempotent: if the requested model is already loaded, just
/// update the settings pin and return. The settings panel calls this
/// whenever its "Local model" dropdown changes; writing to
/// settings.ai.localModel alone isn't enough because that only influences
/// the NEXT boot — the in-memory router still has the old backend.
///
/// Error surface: clear messages for "unknown id" and "not downloaded"
/// so the UI can hint at the fix (run /arcterm-download <id>) instead of
/// showing a generic load failure.
#[tauri::command]
pub async fn ai_set_local_model(
    router: State<'_, std::sync::Arc<AiRouter>>,
    store: State<'_, std::sync::Arc<SettingsStore>>,
    id: String,
) -> Result<(), String> {
    let spec = models::find(&id)
        .ok_or_else(|| format!("unknown model id: {id}"))?;
    if !spec.is_installed() {
        return Err(format!(
            "Model '{id}' is not downloaded. Run /arcterm-download {id} first."
        ));
    }

    // Fast-path: same model already loaded → no-op except for persistence.
    if let Some(current) = router.inner().local_backend() {
        if let Some(current_spec) = current.model_spec() {
            if current_spec.id == id {
                let id_clone = id.clone();
                store.inner().update(move |s| s.ai.local_model = id_clone)?;
                return Ok(());
            }
        }
    }

    let path = spec
        .local_path()
        .ok_or_else(|| "model path unavailable".to_string())?;
    log::info!(
        "swapping local model: id={} path={}",
        spec.id,
        path.display()
    );
    let loaded = tokio::task::spawn_blocking(move || {
        crate::ai::local_llama::LocalLlamaBackend::load(path)
    })
    .await
    .map_err(|e| format!("load join: {e}"))?
    .map_err(|e| format!("local model load: {e}"))?;
    // install_local with switch_to_auto=false: preserve whatever mode
    // the user had; they're changing the backend, not the mode.
    router
        .inner()
        .install_local(std::sync::Arc::new(loaded), false)?;
    let id_clone = id.clone();
    store.inner().update(move |s| s.ai.local_model = id_clone)?;
    Ok(())
}

#[tauri::command]
pub async fn ai_set_mode(
    router: State<'_, std::sync::Arc<AiRouter>>,
    store: State<'_, std::sync::Arc<SettingsStore>>,
    mode: String,
) -> Result<(), String> {
    let parsed = Mode::parse(&mode)?;

    // Lazy-load a local model when switching to Local / Auto.
    //
    // Boot only loads a local model if settings.ai.mode at launch time
    // was Local or Auto — so a user who boots in Claude mode, installs
    // a GGUF via /arcterm-download, then switches to Local would hit
    // "no local model loaded" despite having one on disk. Load on
    // demand here: prefer the pinned id, fall back to any installed
    // entry, surface a clear error only if truly nothing is installed.
    //
    // Loading a 3-8 GB GGUF + compiling Metal shaders is slow; we
    // spawn_blocking so the Tauri event loop stays responsive and the
    // user sees the "thinking" of the UI during load instead of a
    // frozen window.
    if matches!(parsed, Mode::Local | Mode::Auto) && !router.inner().local_available() {
        let pinned_id = store.inner().get().ai.local_model;
        let spec = models::find(&pinned_id)
            .filter(|s| s.is_installed())
            .or_else(|| models::REGISTRY.iter().find(|s| s.is_installed()))
            .ok_or_else(|| {
                "No local model installed. Run `/arcterm-download gemma` first.".to_string()
            })?;
        let path = spec
            .local_path()
            .ok_or_else(|| "model path unavailable".to_string())?;
        log::info!("lazy-loading local model: id={} path={}", spec.id, path.display());
        let loaded = tokio::task::spawn_blocking(move || {
            crate::ai::local_llama::LocalLlamaBackend::load(path)
        })
        .await
        .map_err(|e| format!("load join: {e}"))?
        .map_err(|e| format!("local model load: {e}"))?;
        // install_local with switch_to_auto=false: we'll call set_mode
        // explicitly below with the user's chosen mode.
        router
            .inner()
            .install_local(std::sync::Arc::new(loaded), false)?;
        // Also update settings to reflect which variant we loaded — so
        // next boot eagerly loads this one instead of the (possibly
        // stale) pinned default.
        let resolved_id = spec.id.to_string();
        store.inner().update(move |s| s.ai.local_model = resolved_id)?;
    }

    router.inner().set_mode(parsed)?;
    // Persist so the choice survives relaunch.
    store.inner().update(|s| s.ai.mode = parsed.as_str().to_string())?;
    Ok(())
}

#[tauri::command]
pub fn ai_status(
    router: State<'_, std::sync::Arc<AiRouter>>,
) -> serde_json::Value {
    let router = router.inner();
    let active = router.active();
    // Pull the specific model variant the local backend loaded, when one
    // is loaded. If the loaded path doesn't match a registry entry (user
    // dropped a custom GGUF in ~/.arcterm/models/ — future feature), fall
    // back to the filename so the UI always shows SOMETHING recognizable.
    let local_model = router.local_backend().map(|b| {
        match b.model_spec() {
            Some(spec) => serde_json::json!({
                "id": spec.id,
                "display_name": spec.display_name,
                "quantization": spec.quantization,
                "parameters": spec.parameters,
            }),
            None => {
                let fallback = b
                    .model_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(unknown)")
                    .to_string();
                serde_json::json!({
                    "id": fallback.clone(),
                    "display_name": fallback,
                    "quantization": serde_json::Value::Null,
                    "parameters": serde_json::Value::Null,
                })
            }
        }
    });
    serde_json::json!({
        "mode": router.current_mode().as_str(),
        "active_id": active.id(),
        "active_display_name": active.display_name(),
        "local_available": router.local_available(),
        "local_model": local_model,
    })
}

// -- Filesystem completion ---------------------------------------------

/// Tab-completion for file paths. Given the full editor text + cursor
/// position, returns the token being completed and its candidates. The
/// frontend splices `replacement` into the editor at `[token_start,
/// token_end)` — we return byte offsets so it doesn't have to re-derive.
#[tauri::command]
pub fn fs_complete(
    text: String,
    cursor_pos: usize,
    cwd: Option<String>,
) -> Result<CompletionResult, String> {
    // cwd is None if the shell hasn't reported OSC 7 yet. Fall back to the
    // process cwd — better than failing outright; the user may still get
    // useful completions relative to wherever ArcTerm was launched.
    let cwd_path = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| "no cwd available".to_string())?;
    Ok(fs_complete_impl(&text, cursor_pos, &cwd_path))
}
