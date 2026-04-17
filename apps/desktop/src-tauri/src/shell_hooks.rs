//! Shell integration installer.
//!
//! ArcTerm needs its zsh hooks (prompt suppression + OSC 7 cwd reporting)
//! on disk so `ZDOTDIR=...` can point zsh at them. We embed the scripts at
//! compile time with `include_str!` and extract them to `~/.arcterm/` on
//! every app launch — overwriting unconditionally so the files always match
//! the shipped version (users shouldn't edit these; they should edit their
//! own `~/.zshrc`).
//!
//! Layout written to disk:
//!
//!   ~/.arcterm/
//!     shell-integration/
//!       arcterm.zsh         -- the actual hooks
//!     zdotdir/
//!       .zshenv             -- re-sources user's $HOME/.zshenv
//!       .zshrc              -- sources user's $HOME/.zshrc then arcterm.zsh
//!
//! When the PTY spawns, we export `ZDOTDIR=~/.arcterm/zdotdir` so zsh reads
//! our rc files (which themselves chain-load the user's). We also export
//! `ARCTERM_INTEGRATION_DIR` so `.zshrc` can find `arcterm.zsh` even if the
//! user has ever moved the install directory.

use std::fs;
use std::path::{Path, PathBuf};

// Paths are relative to this source file:
//   src/shell_hooks.rs -> up 4 levels -> workspace root
const ARCTERM_ZSH: &str = include_str!("../../../../shell-integration/arcterm.zsh");
const ZDOTDIR_ZSHENV: &str = include_str!("../../../../shell-integration/zdotdir/.zshenv");
const ZDOTDIR_ZSHRC: &str = include_str!("../../../../shell-integration/zdotdir/.zshrc");

/// Paths the PTY spawner needs. Returned so we can set env vars without
/// re-computing them.
pub struct Paths {
    /// Value for `ZDOTDIR`. zsh reads `.zshenv`, `.zshrc` etc. from here.
    pub zdotdir: PathBuf,
    /// Value for `ARCTERM_INTEGRATION_DIR`. The `.zshrc` we write uses this
    /// to locate `arcterm.zsh` so users can't accidentally break sourcing
    /// by moving files around.
    pub integration_dir: PathBuf,
}

/// Install (or refresh) the shell integration files under `~/.arcterm/`.
///
/// Idempotent and cheap — the files together are a few KB and writing them
/// every launch avoids a whole class of "stale script" bugs.
pub fn install() -> Result<Paths, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME env var not set; cannot install shell integration".to_string())?;

    let root = home.join(".arcterm");
    let integration_dir = root.join("shell-integration");
    let zdotdir = root.join("zdotdir");

    fs::create_dir_all(&integration_dir)
        .map_err(|e| format!("create {}: {e}", integration_dir.display()))?;
    fs::create_dir_all(&zdotdir).map_err(|e| format!("create {}: {e}", zdotdir.display()))?;

    write_file(&integration_dir.join("arcterm.zsh"), ARCTERM_ZSH)?;
    write_file(&zdotdir.join(".zshenv"), ZDOTDIR_ZSHENV)?;
    write_file(&zdotdir.join(".zshrc"), ZDOTDIR_ZSHRC)?;

    log::info!(
        "shell integration installed: zdotdir={} integration={}",
        zdotdir.display(),
        integration_dir.display()
    );

    Ok(Paths {
        zdotdir,
        integration_dir,
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    // Write atomically-ish: we don't bother with a temp-rename dance because
    // a partial write here would only affect the next shell spawn, and we
    // rewrite on every launch anyway.
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))
}
