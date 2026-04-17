//! Filesystem path completion.
//!
//! Tab in the input editor invokes `fs_complete` with the current editor
//! text + cursor position. We extract the token under the cursor, split
//! it into directory + basename, and list the directory for entries that
//! start with the basename.
//!
//! Scope (intentional):
//!   - Paths only — file + dir completion, the ~80 % case for Tab in a
//!     terminal. No command-name completion, no argument specs, no
//!     subcommand awareness. Phase 7 polish will add richer sources.
//!   - `~` expansion, absolute + relative paths, cwd-relative.
//!   - Hidden files surfaced only if the user typed a leading `.` in the
//!     basename (matches shell conventions).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Dir,
    File,
    /// Executable regular file (on the filesystem path).
    Executable,
    /// A subcommand of a known CLI tool (e.g. `git checkout`). Sourced
    /// from the command-spec registry, not the filesystem.
    Subcommand,
    /// A flag/option of a known CLI tool (e.g. `--verbose`, `-C`).
    Option,
}

#[derive(Debug, Clone, Serialize)]
pub struct Completion {
    /// Display label, e.g. `Apple/`, `checkout`, or `--verbose`. Trailing
    /// slash on directories so users see at a glance what the entry is.
    pub label: String,
    /// Bytes to insert into the editor, replacing the trailing token. For
    /// directories this ends with `/` so the user can keep tab-completing
    /// into the subtree without an extra keystroke.
    pub replacement: String,
    pub kind: CompletionKind,
    /// True when the entry starts with `.` — styled subtly in the UI.
    #[serde(default)]
    pub hidden: bool,
    /// Human-readable description, if one was sourced (spec-based
    /// completions carry a short explanation of each subcommand / flag;
    /// filesystem completions have none). Shown on the right side of
    /// the dropdown row.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletionResult {
    /// Byte offset inside the original editor text where the completed
    /// token begins. Frontend uses this to splice `replacement` in.
    pub token_start: usize,
    /// Byte offset one past the end of the token. Usually equals cursor_pos,
    /// but we return it explicitly so the frontend doesn't have to re-derive.
    pub token_end: usize,
    pub completions: Vec<Completion>,
}

/// Entry point called from the Tauri command handler.
pub fn complete(text: &str, cursor_pos: usize, cwd: &Path) -> CompletionResult {
    let cursor_pos = cursor_pos.min(text.len());
    let (token_start, token) = extract_token(text, cursor_pos);
    let (dir_part, base) = split_dir_base(token);

    let dir = resolve_dir(&dir_part, cwd);
    let mut completions = match list_dir(&dir, base) {
        Ok(c) => c,
        Err(_) => Vec::new(),
    };

    // Sort: directories first (they're usually what you want to cd into),
    // then alphabetic. Hidden files sort after non-hidden within each group.
    completions.sort_by(|a, b| {
        use std::cmp::Ordering;
        let a_dir = matches!(a.kind, CompletionKind::Dir);
        let b_dir = matches!(b.kind, CompletionKind::Dir);
        match (a_dir, b_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => match (a.hidden, b.hidden) {
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
            },
        }
    });

    // Each completion's replacement is the full dir + matched entry. We
    // build it from the raw `dir_part` (what the user typed) to preserve
    // their style (~/foo vs /Users/.../foo vs relative).
    for c in &mut completions {
        c.replacement = join_preserving_style(&dir_part, &c.replacement);
    }

    CompletionResult {
        token_start,
        token_end: cursor_pos,
        completions,
    }
}

/// Find the token under the cursor. A "token" here is a run of non-whitespace
/// bytes ending at cursor_pos — we don't try to be fancy about shell quoting
/// (the Phase 7 full-shell-lexer can handle that later).
fn extract_token(text: &str, cursor_pos: usize) -> (usize, &str) {
    let bytes = text.as_bytes();
    let mut start = cursor_pos;
    while start > 0 {
        let ch = bytes[start - 1];
        if ch == b' ' || ch == b'\t' || ch == b'\n' {
            break;
        }
        start -= 1;
    }
    // Ensure we land on a UTF-8 char boundary (defensive — the editor only
    // emits well-formed text, but a future extension that lets the caller
    // pass byte-position from an xterm selection might not).
    while start < cursor_pos && !text.is_char_boundary(start) {
        start += 1;
    }
    (start, &text[start..cursor_pos])
}

/// Split a token like `path/to/fi` into (`path/to`, `fi`). For a bare token
/// with no slashes it's ("", token).
fn split_dir_base(token: &str) -> (String, &str) {
    match token.rfind('/') {
        Some(i) => (token[..=i].to_string(), &token[i + 1..]),
        None => (String::new(), token),
    }
}

/// Resolve a user-facing directory spec against the cwd. Expands leading
/// `~` and `~/`. Relative paths are joined onto cwd; absolute paths are
/// kept as-is.
fn resolve_dir(dir_part: &str, cwd: &Path) -> PathBuf {
    if dir_part.is_empty() {
        return cwd.to_path_buf();
    }
    // Tilde expansion. We only handle bare `~` and `~/foo` — `~user/foo`
    // (other users' homes) is rare and requires passwd lookup; skip.
    if let Some(rest) = dir_part.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if dir_part == "~" || dir_part == "~/" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    let p = PathBuf::from(dir_part);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn list_dir(dir: &Path, base: &str) -> std::io::Result<Vec<Completion>> {
    let show_hidden = base.starts_with('.');
    let base_lc = base.to_lowercase();
    let mut out = Vec::new();

    // Read up to a safety cap. Enormous dirs (node_modules, homebrew Cellar)
    // would lag the UI; 2000 entries is plenty for meaningful completion.
    const CAP: usize = 2000;
    for (i, entry) in fs::read_dir(dir)?.enumerate() {
        if i >= CAP {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue, // non-UTF-8 names skipped
        };
        let hidden = name.starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        // Prefix match is case-insensitive. We keep the original case for
        // display and replacement — only the filter is case-fold.
        if !name.to_lowercase().starts_with(&base_lc) {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let kind = if is_dir {
            CompletionKind::Dir
        } else if is_executable(&meta) {
            CompletionKind::Executable
        } else {
            CompletionKind::File
        };
        let label = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        let replacement = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        out.push(Completion {
            label,
            replacement,
            kind,
            hidden,
            // Filesystem entries have no description (spec-based
            // completions do). The dropdown renders "" as an empty
            // meta cell, which is fine.
            description: None,
        });
    }
    Ok(out)
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.is_file() && (meta.permissions().mode() & 0o111) != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

/// Combine the user's original dir string with the matched entry name,
/// preserving their style. If the user typed `~/Code/` we keep the `~`
/// rather than rewriting to an absolute path.
fn join_preserving_style(dir_part: &str, entry_replacement: &str) -> String {
    if dir_part.is_empty() {
        entry_replacement.to_string()
    } else {
        format!("{dir_part}{entry_replacement}")
    }
}
