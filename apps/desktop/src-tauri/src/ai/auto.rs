//! Auto backend: prefer Claude, fall back to local on specific failure modes.
//!
//! Rules for when we fall back to local:
//!   - Claude CLI isn't available (binary not on PATH, not installed).
//!   - Claude returned an auth error (401 / "Not logged in").
//!   - Claude timed out.
//!   - Claude errored with a transport-level issue (network unreachable).
//!
//! Rules for when we do NOT fall back:
//!   - Claude returned a valid answer (obviously).
//!   - Claude explicitly refused (content policy) — we want the user to
//!     know, not silently swap to a model that may answer differently.
//!
//! Implementation note: we detect fallback cases from the stringified
//! error we get back from `ClaudeCliBackend.ask()`. That's a bit brittle
//! but it's what the current error surface gives us — Phase 7 could
//! introduce a proper error enum across the trait if this grows hairy.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::{AiBackend, AiRequest, AiResponse};

pub struct AutoBackend {
    primary: Arc<dyn AiBackend>,
    secondary: Arc<dyn AiBackend>,
}

impl AutoBackend {
    pub fn new(primary: Arc<dyn AiBackend>, secondary: Arc<dyn AiBackend>) -> Self {
        Self { primary, secondary }
    }
}

#[async_trait]
impl AiBackend for AutoBackend {
    fn id(&self) -> &'static str {
        "auto"
    }
    fn display_name(&self) -> &'static str {
        "Auto (Claude → local)"
    }

    async fn is_available(&self) -> bool {
        // "Available" means at least one backend is. Auto mode is the
        // most forgiving — if either primary or secondary works, we can
        // answer.
        self.primary.is_available().await || self.secondary.is_available().await
    }

    async fn ask(&self, req: AiRequest) -> Result<AiResponse, String> {
        // Skip the primary entirely when it's known-unavailable — saves
        // a subprocess spawn + the error round-trip. If the availability
        // check itself is flaky, we still hit the primary and its error
        // routing takes over.
        if !self.primary.is_available().await {
            log::info!(
                "auto: primary {} unavailable, using secondary {}",
                self.primary.id(),
                self.secondary.id()
            );
            return self.secondary.ask(req).await;
        }
        match self.primary.ask(req.clone()).await {
            Ok(resp) => Ok(resp),
            Err(err) if is_fallback_worthy(&err) => {
                log::info!(
                    "auto: primary {} errored ({err}); falling back to secondary {}",
                    self.primary.id(),
                    self.secondary.id()
                );
                self.secondary.ask(req).await
            }
            Err(err) => Err(err),
        }
    }

    fn stream(&self, req: AiRequest) -> BoxStream<'static, Result<String, String>> {
        let primary = self.primary.clone();
        let secondary = self.secondary.clone();

        // Streaming fallback is trickier than ask() because we might
        // start emitting primary's chunks, then hit a fallback-worthy
        // error mid-stream. That would leave the UI with a partial
        // answer. So: check availability first; if primary is down,
        // stream secondary from the start. If primary is up, stream it
        // without mid-stream fallback — errors propagate to the UI
        // (which can present a "retry on local" button later).
        let (tx, rx) = mpsc::unbounded_channel::<Result<String, String>>();
        tokio::spawn(async move {
            let chosen = if primary.is_available().await {
                primary
            } else {
                secondary
            };
            let mut stream = chosen.stream(req);
            while let Some(item) = stream.next().await {
                if tx.send(item).is_err() {
                    return;
                }
            }
        });
        UnboundedReceiverStream::new(rx).boxed()
    }
}

/// Heuristic: should we retry on the secondary given this error string?
fn is_fallback_worthy(err: &str) -> bool {
    let e = err.to_lowercase();
    // Auth errors — subscription not logged in, key invalid, etc.
    if e.contains("401") || e.contains("not logged in") || e.contains("authentication") {
        return true;
    }
    // Transport failures — network dead, DNS.
    if e.contains("timed out")
        || e.contains("timeout")
        || e.contains("failed to connect")
        || e.contains("dns")
        || e.contains("network")
    {
        return true;
    }
    // Rate limits — trying local avoids the hard-fail for the user.
    if e.contains("429") || e.contains("rate limit") {
        return true;
    }
    // Everything else (content policy refusals, bad request shapes) —
    // surface to the user so they can fix their prompt.
    false
}
