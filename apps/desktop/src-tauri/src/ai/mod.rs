//! AI backend abstraction.
//!
//! Phase 5a ships one backend (`ClaudeCliBackend`). Phase 5b will add a
//! `LocalLlamaBackend` and an `AutoBackend` that falls back from Claude to
//! local when Claude is unavailable. All of them implement the same trait
//! so Tauri command handlers don't know or care which is active.
//!
//! Why async-trait? The backends spawn subprocesses (Claude CLI) and will
//! eventually call into native inference libs (llama_cpp_2). Both are
//! naturally async. `async_trait::async_trait` is the standard workaround
//! until the native async-in-traits story is complete enough to be
//! ergonomic for dyn Trait uses like ours.

pub mod claude;
pub mod context;
pub mod router;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub use context::AiContext;
pub use router::AiRouter;

/// Structured prompt handed to a backend. Keeping this a struct (not a raw
/// string) lets us add structured fields (tool calls, attachments, agent
/// state) later without breaking the trait signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRequest {
    /// Primary instruction text — e.g. "Convert to a shell command: find all
    /// python files modified in the last week".
    pub prompt: String,
    /// Context gathered on the Rust side (cwd, shell, recent history, etc).
    /// None means the caller is fine with no grounding.
    #[serde(default)]
    pub context: Option<AiContext>,
    /// Hint to the backend about what kind of answer is expected. Helps the
    /// UI decide how to render (command vs prose vs structured).
    #[serde(default)]
    pub mode: AiMode,
}

/// Response kinds the UI knows how to render. Keep the variants small and
/// explicit — we want a tight contract with the frontend, not a sprawling
/// "metadata" bag.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiMode {
    /// Default chatty answer (markdown-capable prose).
    #[default]
    Chat,
    /// "Convert this natural language to a shell command" — answer should
    /// be a single line, no prose, ready to execute.
    Command,
    /// "Explain this error and suggest a fix" — answer is prose plus an
    /// optional one-line fix command at the end.
    Explain,
}

/// Final, non-streaming response. Used by `AiBackend::ask`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiResponse {
    /// The text response. For Command mode, this should be just the command.
    pub text: String,
    /// Which backend produced this. Useful in the UI to show "via Claude"
    /// vs "via local Gemma" so the user understands latency/quality.
    pub backend: String,
}

/// A chunk of a streaming response. Emitted via Tauri events; the frontend
/// stitches them into the final text. `done` is true on the final chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiChunk {
    /// Request id (uuid) so the frontend can route chunks to the right
    /// in-flight call when multiple are live.
    pub id: String,
    /// Incremental text to append. Empty on the final "done" chunk.
    pub delta: String,
    /// True exactly once per request, on the last chunk.
    pub done: bool,
    /// Non-empty only on the done chunk if something went wrong.
    #[serde(default)]
    pub error: Option<String>,
}

/// The trait all backends implement. Object-safe via async_trait; Send +
/// Sync so we can hold `Arc<dyn AiBackend>` inside Tauri state across
/// threads and await handles.
#[async_trait]
pub trait AiBackend: Send + Sync {
    /// Stable machine-readable id, e.g. "claude-cli" or "local-gemma".
    /// The frontend uses this to pick the right UI affordances and to
    /// display which backend answered.
    fn id(&self) -> &'static str;

    /// Human-readable name for UI (settings dropdown, "answered by …").
    fn display_name(&self) -> &'static str;

    /// Quick check without making a real request. Used to gate AI UI and
    /// by the auto router's fallback logic. Should be near-instant — don't
    /// spawn subprocesses just to check availability.
    async fn is_available(&self) -> bool;

    /// Single-shot, blocking-ish: return the full answer when done. Best
    /// for short prompts (command generation). Chat/explain prefer stream().
    async fn ask(&self, req: AiRequest) -> Result<AiResponse, String>;

    /// Streaming: yield text chunks as they arrive. The stream must end
    /// (either with success or with an `Err(...)` item). Consumer in
    /// `ipc.rs` wraps each chunk into an `ai://chunk` Tauri event.
    fn stream(&self, req: AiRequest) -> BoxStream<'static, Result<String, String>>;
}
