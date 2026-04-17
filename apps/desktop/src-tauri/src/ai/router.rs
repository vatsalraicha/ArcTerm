//! Router: orchestrates which backend answers requests.
//!
//! State:
//!   - `claude`:    the Claude CLI backend (always present; availability
//!                  is checked lazily via is_available()).
//!   - `local`:     the local Gemma backend, loaded from a GGUF file.
//!                  `None` when no model is installed or the model file
//!                  failed to load at startup.
//!   - `active`:    the currently-dispatched backend. Swapped atomically
//!                  by `set_mode`. Starts as whatever the settings file
//!                  says, falling back when the requested backend isn't
//!                  actually available on this machine.
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
        }
    }

    /// Swap the active backend. Returns an error if the requested mode
    /// can't be satisfied (e.g. "local" with no loaded model).
    pub fn set_mode(&self, mode: Mode) -> Result<(), String> {
        let local = self.local.read().clone();
        match (mode, &local) {
            (Mode::Local, None) => {
                return Err(
                    "No local model loaded. Run `/arcterm-download gemma` first.".into(),
                );
            }
            _ => {}
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
                &*self.local.read(),
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
