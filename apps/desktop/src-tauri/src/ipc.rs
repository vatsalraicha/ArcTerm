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
    settings: Settings,
) -> Result<(), String> {
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
        let router = router.inner().clone();
        let load_result = tokio::task::spawn_blocking(move || {
            crate::ai::local_llama::LocalLlamaBackend::load(path)
        })
        .await
        .map_err(|e| format!("load join: {e}"))?;

        match load_result {
            Ok(backend) => {
                // Switch mode to Auto so the newly-available local model
                // actually gets used in the absence of Claude.
                let _ = router.install_local(std::sync::Arc::new(backend), true);
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

#[tauri::command]
pub fn ai_set_mode(
    router: State<'_, std::sync::Arc<AiRouter>>,
    store: State<'_, std::sync::Arc<SettingsStore>>,
    mode: String,
) -> Result<(), String> {
    let parsed = Mode::parse(&mode)?;
    router.inner().set_mode(parsed)?;
    // Persist so the choice survives relaunch.
    store.inner().update(|s| s.ai.mode = parsed.as_str().to_string())?;
    Ok(())
}

#[tauri::command]
pub fn ai_status(
    router: State<'_, std::sync::Arc<AiRouter>>,
) -> serde_json::Value {
    let active = router.inner().active();
    serde_json::json!({
        "mode": router.inner().current_mode().as_str(),
        "active_id": active.id(),
        "active_display_name": active.display_name(),
        "local_available": router.inner().local_available(),
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
