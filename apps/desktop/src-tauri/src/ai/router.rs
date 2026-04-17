//! Router: holds the currently-selected backend and dispatches requests.
//!
//! Today (Phase 5a) this is thin — one backend, no fallback. Phase 5b adds:
//!   - multiple registered backends keyed by id ("claude-cli", "local-gemma")
//!   - a "mode" (claude | local | auto) stored in user config
//!   - auto's fallback rules (Claude first, local if CLI missing/errored)
//!
//! The router owns backends as `Arc<dyn AiBackend>` so we can read them
//! inside an `async` command handler without holding a lock across await
//! points.

use std::sync::Arc;

use parking_lot::RwLock;

use super::{AiBackend, AiRequest, AiResponse};

pub struct AiRouter {
    /// The active backend. Swappable at runtime via `set_active` so settings
    /// UI (Phase 5b) can switch without restarting the app.
    active: RwLock<Arc<dyn AiBackend>>,
}

impl AiRouter {
    pub fn new(initial: Arc<dyn AiBackend>) -> Self {
        Self {
            active: RwLock::new(initial),
        }
    }

    #[allow(dead_code)]
    pub fn set_active(&self, backend: Arc<dyn AiBackend>) {
        *self.active.write() = backend;
    }

    pub fn active(&self) -> Arc<dyn AiBackend> {
        self.active.read().clone()
    }

    pub async fn is_available(&self) -> bool {
        self.active().is_available().await
    }

    pub async fn ask(&self, req: AiRequest) -> Result<AiResponse, String> {
        self.active().ask(req).await
    }

    // Streaming goes through the command handler directly — callers want
    // to own the stream to drive Tauri event emission, so we don't wrap
    // it here. `active()` exposes the backend if they need it.
}
