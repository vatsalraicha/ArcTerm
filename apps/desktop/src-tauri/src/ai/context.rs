//! Context bundled with every AI request.
//!
//! The quality of an AI answer for a terminal task is mostly a function of
//! the context we feed it: current directory, recent commands, git state,
//! failing command + stderr. We collect these cheaply on the Rust side
//! rather than asking the frontend to marshal them across IPC, because
//! some of it (process-local env, directory listing) is inherently
//! server-side.
//!
//! Budget-awareness: prompts get billed by token. `build_prompt_block`
//! truncates long fields and caps the total so we never ship megabytes to
//! Claude for a trivial question.

use serde::{Deserialize, Serialize};

use crate::history::HistoryStore;

const MAX_HISTORY_ENTRIES: usize = 10;
const MAX_DIRECTORY_BYTES: usize = 1_500;
const MAX_ERROR_BYTES: usize = 2_500;

/// Everything the caller wants to ground the AI answer in. Fields are
/// optional because callers may only have some of them (e.g. Cmd+K without
/// an error context still wants cwd + recent history but has no stderr).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiContext {
    /// Absolute cwd (active session's).
    pub cwd: Option<String>,
    /// User's shell path, e.g. /bin/zsh.
    pub shell: Option<String>,
    /// Git branch, if applicable. Empty string means "not in a repo".
    pub git_branch: Option<String>,
    /// Up to MAX_HISTORY_ENTRIES most recent commands (in cwd if possible).
    #[serde(default)]
    pub recent_commands: Vec<String>,
    /// The command whose output/error is the subject of this request.
    /// Populated by the explain flow; empty for "generate a command" flow.
    pub failing_command: Option<String>,
    /// Captured stderr (or combined stdout+stderr tail) of `failing_command`.
    pub failing_output: Option<String>,
    /// Exit code of `failing_command`. Used to render "Exit 127" in the
    /// prompt so the model knows *how* the command failed.
    pub failing_exit_code: Option<i64>,
}

impl AiContext {
    /// Minimal context block: cwd, shell, OS, git branch. No history.
    ///
    /// Used by small local models that struggle with longer preambles —
    /// at 2-bit quantization a 2B-param model treats the recent-commands
    /// list as "examples of what to output" instead of "background
    /// context", which produces pattern-matched one-word answers.
    /// Frontier models (Claude) don't have this problem, so they keep
    /// the full block.
    pub fn to_compact_prompt_block(&self) -> String {
        let mut out = String::new();
        out.push_str("## Context\n");
        if let Some(cwd) = &self.cwd {
            out.push_str(&format!("- Working directory: {cwd}\n"));
        }
        out.push_str(&format!(
            "- Shell: {}\n",
            self.shell.as_deref().unwrap_or("/bin/zsh")
        ));
        out.push_str("- OS: macOS\n");
        if let Some(branch) = &self.git_branch {
            if !branch.is_empty() {
                out.push_str(&format!("- Git branch: {branch}\n"));
            }
        }
        // Failing command + output are preserved even in compact mode:
        // explain flows NEED this to give a useful answer.
        if let (Some(cmd), Some(output)) = (&self.failing_command, &self.failing_output) {
            out.push_str(&format!("\nCommand that failed: `{cmd}`\n"));
            if let Some(code) = self.failing_exit_code {
                out.push_str(&format!("Exit code: {code}\n"));
            }
            out.push_str("Output:\n```\n");
            out.push_str(truncate_bytes(output, MAX_ERROR_BYTES));
            if !output.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out
    }

    /// Render the context as a prompt fragment for text-based LLMs. The
    /// resulting block is injected before the user's question so the model
    /// can ground its answer without us doing tool-use plumbing.
    ///
    /// Format mirrors what most instruction-tuned models respond well to:
    /// a short preamble followed by labeled sections.
    pub fn to_prompt_block(&self) -> String {
        let mut out = String::new();
        out.push_str("## Context\n");

        if let Some(cwd) = &self.cwd {
            out.push_str(&format!("- Working directory: {cwd}\n"));
        }
        if let Some(shell) = &self.shell {
            out.push_str(&format!("- Shell: {shell}\n"));
        } else {
            out.push_str("- Shell: /bin/zsh\n");
        }
        out.push_str("- OS: macOS\n");
        if let Some(branch) = &self.git_branch {
            if !branch.is_empty() {
                out.push_str(&format!("- Git branch: {branch}\n"));
            }
        }
        if !self.recent_commands.is_empty() {
            out.push_str("\n### Recent commands\n```\n");
            for cmd in &self.recent_commands {
                out.push_str(cmd);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        if let (Some(cmd), Some(output)) = (&self.failing_command, &self.failing_output) {
            out.push_str("\n### Command output\n");
            out.push_str(&format!("Command: `{cmd}`\n"));
            if let Some(code) = self.failing_exit_code {
                out.push_str(&format!("Exit code: {code}\n"));
            }
            out.push_str("Output:\n```\n");
            out.push_str(truncate_bytes(output, MAX_ERROR_BYTES));
            if !output.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out
    }
}

/// Build the richest context we can from the Rust side. Callers give us
/// whatever they already know (cwd, branch, failing command/output) and we
/// fill in the rest (shell, recent history, dir listing).
///
/// Purely additive to the seed — we never overwrite fields the caller set.
pub fn enrich(
    seed: AiContext,
    history: Option<&HistoryStore>,
) -> AiContext {
    let mut ctx = seed;

    if ctx.shell.is_none() {
        ctx.shell = std::env::var("SHELL").ok();
    }

    // Pull recent commands — prefer same-cwd ones, fall back to any.
    if ctx.recent_commands.is_empty() {
        if let Some(store) = history {
            let entries = store
                .search("", ctx.cwd.as_deref(), MAX_HISTORY_ENTRIES as u32)
                .unwrap_or_default();
            ctx.recent_commands = entries
                .into_iter()
                .map(|e| e.command)
                .rev() // oldest-first is more natural in a prompt
                .collect();
        }
    }

    // Directory listing: stored inside failing_output only when the caller
    // wanted it — otherwise we leave it off, because `ls -la` on a large
    // node_modules is pure noise. Phase 5b may add a selective `tree -L 1`.

    ctx
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Trim from the start: the most relevant part of a long error is
    // usually the tail (the actual failure, not the lead-in noise).
    // Safe char boundary: find the last UTF-8 char boundary at or before
    // `s.len() - max`.
    let tail_start = s.len().saturating_sub(max);
    let mut start = tail_start;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[allow(dead_code)]
pub const MAX_DIR_BYTES_FOR_TESTS: usize = MAX_DIRECTORY_BYTES;
