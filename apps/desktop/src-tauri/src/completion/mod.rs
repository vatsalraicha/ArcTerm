//! Tab-completion subsystem.
//!
//! Two sources of completions live here:
//!
//!   - **Filesystem** (`fs`): path completion for any command. The only
//!     source before Phase 7. Still handles the common case — `cd App<Tab>`,
//!     `cat src/te<Tab>`, etc.
//!
//!   - **Command specs** (`specs`): subcommand + option completion for a
//!     curated set of CLIs imported from the Fig autocomplete project.
//!     Fires when the editor's first token is a known command and the
//!     cursor sits past that first word.
//!
//! `complete()` is the single entry point the IPC layer calls; it picks
//! which source to dispatch to based on where the cursor lives in the
//! editor's token sequence.

mod fs;
pub mod specs;

pub use fs::{Completion, CompletionKind, CompletionResult};

use std::path::Path;

/// Entry point called from the Tauri command handler. Routes between
/// filesystem and command-spec completion sources based on what the user
/// is typing.
pub fn complete(text: &str, cursor_pos: usize, cwd: &Path) -> CompletionResult {
    // Token analysis: if the cursor is past the FIRST whitespace-delimited
    // word AND that first word matches a known Fig spec, we're completing
    // arguments to that CLI — use spec-based completion. Otherwise fall
    // back to filesystem completion (paths, first-token command names).
    if let Some(spec_result) = specs::try_complete(text, cursor_pos) {
        return spec_result;
    }
    fs::complete(text, cursor_pos, cwd)
}
