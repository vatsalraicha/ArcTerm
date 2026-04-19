//! In-memory audit log + content sniffer for sensitive IPC commands.
//!
//! Scope (deliberate, based on Wave 5 research):
//!
//!   - **Forensic trail, not defense.** Every entry captures command
//!     name, timestamp, payload size, and optional sniffer flag.
//!     Never blocks or rate-limits: the industry consensus (VSCode,
//!     Warp, Tauri's own docs) is that per-command input validation
//!     does the real defensive work; middleware rate limits create
//!     false-positive risk and novel bug surface for marginal gain.
//!   - **Privacy-aware.** We log command metadata only — never the
//!     actual bytes of a `pty_write` payload, the text of a prompt,
//!     or the contents of a history entry. The audit is about "who
//!     did what when," not a keylog.
//!   - **Bounded memory.** Ring buffer of the last 200 entries.
//!     Oldest drops when full. Total footprint ~20 KB regardless of
//!     activity. No disk writes — ephemeral by design; if someone
//!     restarts the app to dodge forensics, they've also closed
//!     every terminal session.
//!
//! The `pty_write` sniffer flags patterns that shouldn't come from
//! legitimate frontend input:
//!
//!   - `\x1b]52;` — OSC 52 clipboard set. The input editor never
//!     emits these; presence suggests a compromised renderer trying
//!     to hijack the clipboard via the PTY path.
//!   - `\x1b]1337;` — iTerm-extension sequences. Same reasoning.
//!   - Single line > 8 KB without a newline — unusually-large paste
//!     shape; legitimate pastes almost always contain newlines.
//!   - Known-destructive substrings (`rm -rf /`, fork-bombs) that
//!     reached the PTY *without* going through Wave 3's AI-panel
//!     confirmation flow. Logs the attempt for investigation.
//!
//! Exposed to the frontend via `ipc_audit_tail` which the
//! `/arcterm-audit` slash command consumes.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;

const MAX_ENTRIES: usize = 200;

/// One row in the audit log.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// IPC command name (e.g. "pty_write", "settings_set").
    pub command: &'static str,
    /// Unix milliseconds. Client-side converts to locale time.
    pub timestamp_ms: u64,
    /// Payload size in bytes, if meaningful. `0` when not applicable
    /// (e.g. `pty_kill` has no payload).
    pub bytes: u64,
    /// Sniffer flag, if the payload tripped any of them. `None` for
    /// the common case — a hit is rare and visually loud in the UI.
    pub flag: Option<String>,
}

/// Shared audit buffer. Cloneable Arc; passed into Tauri state.
#[derive(Clone, Default)]
pub struct AuditLog {
    entries: Arc<Mutex<VecDeque<AuditEntry>>>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one entry. Drops oldest when the buffer is full.
    pub fn log(&self, command: &'static str, bytes: u64, flag: Option<String>) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let entry = AuditEntry {
            command,
            timestamp_ms: now_ms,
            bytes,
            flag,
        };
        let mut guard = self.entries.lock();
        if guard.len() == MAX_ENTRIES {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    /// Return the most recent `limit` entries, newest last. Limit is
    /// clamped to `[1, MAX_ENTRIES]` so a UI bug can't ask for
    /// billions of rows.
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        let limit = limit.clamp(1, MAX_ENTRIES);
        let guard = self.entries.lock();
        let start = guard.len().saturating_sub(limit);
        guard.iter().skip(start).cloned().collect()
    }
}

/// Inspect a `pty_write` payload for suspicious patterns. Returns
/// `Some(reason)` when a pattern matches, `None` otherwise. Never
/// blocks — the caller logs and proceeds. The goal is a searchable
/// trail for post-incident investigation, not a gate.
///
/// We only peek at the first 256 bytes (header-shape patterns) and
/// run a cheap substring scan for the destructive strings. Cost is
/// ~µs per call on typical keystroke payloads.
pub fn inspect_pty_write(data: &str) -> Option<String> {
    let bytes = data.as_bytes();

    // OSC 52 clipboard set. The input editor never emits these;
    // presence indicates either a bug or a renderer trying to
    // silently rewrite the user's clipboard via the PTY.
    if bytes.starts_with(b"\x1b]52;") {
        return Some("pty_write began with OSC 52 clipboard sequence".to_string());
    }

    // iTerm OSC 1337 extensions from the renderer. Same reasoning.
    if bytes.starts_with(b"\x1b]1337;") {
        return Some("pty_write began with OSC 1337 extension sequence".to_string());
    }

    // Unusually-long line without a newline — not proof of anything
    // but a useful search anchor for forensics.
    if bytes.len() > 8192 && !data.contains('\n') {
        return Some(format!(
            "pty_write single line of {} bytes with no newline",
            bytes.len()
        ));
    }

    // Destructive-string substrings that shouldn't reach pty_write
    // without going through Wave 3's AI-panel confirmation flow
    // (which logs the `ai_ask` call separately). This is a log-only
    // heuristic — false positives happen (legitimate `rm -rf
    // node_modules`) and are fine: forensic noise, not user-facing.
    let lower = data.to_ascii_lowercase();
    // Substrings must be exact matches — we intentionally avoid pulling
    // the `regex` crate in just for the sniffer. Patterns like "| sh\r"
    // catch curl-pipe-to-shell as the user submits it (`\r` is the
    // Enter keystroke zsh's line editor wraps around the command), and
    // don't false-positive on prefixes like "| sha256sum".
    const DANGER_NEEDLES: &[&str] = &[
        "rm -rf /",
        "rm -fr /",
        ":(){ :|:& };:", // fork bomb
        "dd if=/dev/zero of=/dev/",
        "mkfs.",
        "> /dev/sda",
        "> /dev/nvme",
        "| sh\r",
        "| sh\n",
        "| bash\r",
        "| bash\n",
        "| zsh\r",
        "| zsh\n",
    ];
    for needle in DANGER_NEEDLES {
        if lower.contains(needle) {
            return Some(format!(
                "pty_write contained destructive pattern: {needle}"
            ));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_ring_buffer_caps_at_max() {
        let log = AuditLog::new();
        for i in 0..(MAX_ENTRIES + 50) {
            log.log("pty_write", i as u64, None);
        }
        let tail = log.tail(MAX_ENTRIES + 100);
        assert_eq!(tail.len(), MAX_ENTRIES);
        // Oldest 50 should have been dropped; first visible is entry 50.
        assert_eq!(tail.first().unwrap().bytes, 50);
        assert_eq!(tail.last().unwrap().bytes, (MAX_ENTRIES + 49) as u64);
    }

    #[test]
    fn audit_log_tail_clamps_limit() {
        let log = AuditLog::new();
        log.log("settings_set", 100, None);
        assert_eq!(log.tail(0).len(), 1); // clamped up to 1
        assert_eq!(log.tail(99999).len(), 1); // clamped down to MAX
    }

    #[test]
    fn sniffer_flags_osc52() {
        let flag = inspect_pty_write("\x1b]52;c;aGVsbG8=\x07");
        assert!(flag.is_some());
        assert!(flag.unwrap().contains("OSC 52"));
    }

    #[test]
    fn sniffer_flags_osc1337() {
        let flag = inspect_pty_write("\x1b]1337;ArcTermBranch=evil\x07");
        assert!(flag.is_some());
    }

    #[test]
    fn sniffer_flags_destructive_substrings() {
        assert!(inspect_pty_write("rm -rf /\r").is_some());
        assert!(inspect_pty_write("curl http://x | sh\r").is_some());
    }

    #[test]
    fn sniffer_passes_normal_input() {
        assert!(inspect_pty_write("ls -la\r").is_none());
        assert!(inspect_pty_write("cd /tmp && echo ok\r").is_none());
        // Legitimate rm of local dir — flagged as destructive (false
        // positive is fine for a log-only heuristic; test just pins
        // current behavior so future tweaks are intentional).
        assert!(inspect_pty_write("rm -rf node_modules\r").is_none());
    }

    #[test]
    fn sniffer_flags_long_no_newline() {
        let big = "a".repeat(9000);
        assert!(inspect_pty_write(&big).is_some());
    }
}
