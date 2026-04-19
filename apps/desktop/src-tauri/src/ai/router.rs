//! Router: orchestrates which backend answers requests.
//!
//! State:
//! - `claude`:    the Claude CLI backend (always present; availability
//!   is checked lazily via is_available()).
//! - `local`:     the local Gemma backend, loaded from a GGUF file.
//!   `None` when no model is installed or the model file failed to
//!   load at startup.
//! - `active`:    the currently-dispatched backend. Swapped atomically
//!   by `set_mode`. Starts as whatever the settings file says, falling
//!   back when the requested backend isn't actually available on this
//!   machine.
//!
//! Runtime switching: the `/arcterm-model` slash command and the settings
//! panel both funnel into `set_mode(mode)`. We validate the requested
//! mode against what's physically available — asking for "local" with
//! no model installed reports back an error the UI can surface.

use std::sync::Arc;

use parking_lot::RwLock;

use super::auto::AutoBackend;
use super::claude::ClaudeCliBackend;
use super::local_llama::LocalLlamaBackend;
use super::{AiBackend, AiRequest, AiResponse};

/// Opaque mode value. String in the on-disk config, enum here so code
/// paths can't drift from what the config accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Claude,
    Local,
    Auto,
}

/// Snapshot of which model is being background-loaded right now. Lives on
/// the router so any handler (and `ai_status`) can surface the state.
#[derive(Debug, Clone)]
pub struct LoadingInfo {
    pub id: String,
    pub display_name: String,
    pub quantization: String,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "claude" => Ok(Mode::Claude),
            "local" => Ok(Mode::Local),
            "auto" => Ok(Mode::Auto),
            other => Err(format!(
                "unknown AI mode '{other}' (expected claude|local|auto)"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Claude => "claude",
            Mode::Local => "local",
            Mode::Auto => "auto",
        }
    }
}

pub struct AiRouter {
    claude: Arc<dyn AiBackend>,
    local: RwLock<Option<Arc<LocalLlamaBackend>>>,
    active: RwLock<Arc<dyn AiBackend>>,
    mode: RwLock<Mode>,
    /// Background boot-load state. Set to Some(info) while the Wave 2.5
    /// post-window setup task is hashing + mmap'ing the GGUF; cleared
    /// to None on success (install_local replaces it) or on failure.
    /// The AI panel + toolbar status pill read this to show the right
    /// "loading" / "ready" / "failed" text.
    loading: RwLock<Option<LoadingInfo>>,
    /// SECURITY FIX (#2, TOCTOU): serializes every load / unload / delete
    /// on the local model slot. Previously `ai_set_local_model`,
    /// `ai_set_mode`'s lazy-load branch, `model_download`'s post-download
    /// load, and `model_delete` each did their own `is_installed()` check
    /// before calling `LocalLlamaBackend::load(path)` — the window between
    /// check and load let a concurrent `model_delete` remove the file
    /// mid-load (→ mmap of partially-written bytes → SIGBUS), or let a
    /// concurrent `model_download` replace the file under the loader.
    /// Holding this lock across the whole check-then-act sequence closes
    /// the race. tokio::sync::Mutex because handlers are async and hold
    /// the guard across `spawn_blocking(...).await`.
    load_lock: tokio::sync::Mutex<()>,
}

impl AiRouter {
    /// Build the router with Claude always present and Local optional.
    /// The initial `mode` is validated + clamped: if the caller asks for
    /// Local but no model is loaded, we fall back to Claude.
    pub fn new(
        claude: Arc<dyn AiBackend>,
        local: Option<Arc<LocalLlamaBackend>>,
        desired_mode: Mode,
    ) -> Self {
        let resolved = resolve(desired_mode, &claude, &local);
        let active = build_active(resolved, &claude, &local);
        Self {
            claude,
            local: RwLock::new(local),
            active: RwLock::new(active),
            mode: RwLock::new(resolved),
            loading: RwLock::new(None),
            load_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Mark a background load as in-flight. Used by the Wave 2.5 boot
    /// path so the toolbar pill + ai_status can report "loading…".
    pub fn set_loading(&self, info: Option<LoadingInfo>) {
        *self.loading.write() = info;
    }

    /// Snapshot the current background-load state, if any. Returns None
    /// when nothing is loading (either because the load finished, failed,
    /// or was never started — the caller discriminates via other fields
    /// on `ai_status`).
    pub fn loading_info(&self) -> Option<LoadingInfo> {
        self.loading.read().clone()
    }

    /// Acquire the serialization lock for load / unload / delete operations.
    /// Callers MUST hold the returned guard across the whole
    /// `is_installed → LocalLlamaBackend::load → install_local` (or
    /// equivalent) sequence so concurrent handlers can't race.
    pub async fn lock_loads(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.load_lock.lock().await
    }

    /// Swap the active backend. Returns an error if the requested mode
    /// can't be satisfied (e.g. "local" with no loaded model).
    pub fn set_mode(&self, mode: Mode) -> Result<(), String> {
        let local = self.local.read().clone();
        if let (Mode::Local, None) = (mode, &local) {
            return Err(
                "No local model loaded. Run `/arcterm-download gemma` first.".into(),
            );
        }
        let next = build_active(mode, &self.claude, &local);
        *self.active.write() = next;
        *self.mode.write() = mode;
        log::info!("ai router mode = {}", mode.as_str());
        Ok(())
    }

    /// Attach a freshly-loaded local backend and (optionally) switch to it.
    /// Called by the download flow once the GGUF is on disk and loaded.
    pub fn install_local(
        &self,
        local: Arc<LocalLlamaBackend>,
        switch_to_auto: bool,
    ) -> Result<(), String> {
        *self.local.write() = Some(local);
        if switch_to_auto {
            self.set_mode(Mode::Auto)?;
        } else {
            // Rebuild active in case we were in auto/local without a
            // secondary — now we have one.
            let current_mode = *self.mode.read();
            let next = build_active(
                current_mode,
                &self.claude,
                &self.local.read(),
            );
            *self.active.write() = next;
        }
        Ok(())
    }

    /// Detach a previously-loaded local backend (e.g. after model delete).
    pub fn uninstall_local(&self) {
        *self.local.write() = None;
        // If we were in Local or Auto, degrade to Claude.
        let fallback = resolve(*self.mode.read(), &self.claude, &None);
        let next = build_active(fallback, &self.claude, &None);
        *self.active.write() = next;
        *self.mode.write() = fallback;
    }

    pub fn active(&self) -> Arc<dyn AiBackend> {
        self.active.read().clone()
    }

    pub fn current_mode(&self) -> Mode {
        *self.mode.read()
    }

    pub fn local_available(&self) -> bool {
        self.local.read().is_some()
    }

    /// Borrow the loaded LocalLlamaBackend, if any. Used by ai_status to
    /// surface the specific model variant (e.g. "Gemma 4 E2B Q4_K_M")
    /// rather than just "local".
    pub fn local_backend(&self) -> Option<Arc<LocalLlamaBackend>> {
        self.local.read().clone()
    }

    pub async fn is_available(&self) -> bool {
        self.active().is_available().await
    }

    pub async fn ask(&self, req: AiRequest) -> Result<AiResponse, String> {
        self.active().ask(req).await
    }
}

/// Clamp a requested mode to what's actually possible. Auto falls back
/// to whatever single backend we have; Local falls back to Claude when
/// no local model is installed.
fn resolve(
    desired: Mode,
    _claude: &Arc<dyn AiBackend>,
    local: &Option<Arc<LocalLlamaBackend>>,
) -> Mode {
    match (desired, local.is_some()) {
        (Mode::Local, false) => Mode::Claude,
        (Mode::Auto, false) => Mode::Claude,
        (m, _) => m,
    }
}

/// Build the currently-active dyn-trait object for a given mode.
fn build_active(
    mode: Mode,
    claude: &Arc<dyn AiBackend>,
    local: &Option<Arc<LocalLlamaBackend>>,
) -> Arc<dyn AiBackend> {
    match mode {
        Mode::Claude => claude.clone(),
        Mode::Local => local
            .as_ref()
            .cloned()
            .map(|l| l as Arc<dyn AiBackend>)
            .unwrap_or_else(|| claude.clone()),
        Mode::Auto => match local {
            Some(l) => Arc::new(AutoBackend::new(
                claude.clone(),
                l.clone() as Arc<dyn AiBackend>,
            )),
            None => claude.clone(),
        },
    }
}

// -- Convenience constructor for Phase 5a-style single-backend setup.
// Preserved so Phase 5a migration lands cleanly.
impl AiRouter {
    #[allow(dead_code)]
    pub fn claude_only() -> Self {
        let claude: Arc<dyn AiBackend> = Arc::new(ClaudeCliBackend::default());
        Self::new(claude, None, Mode::Claude)
    }
}
