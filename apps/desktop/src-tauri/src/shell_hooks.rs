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
const ARCTERM_BASH: &str = include_str!("../../../../shell-integration/arcterm.bash");
const ARCTERM_FISH: &str = include_str!("../../../../shell-integration/arcterm.fish");
const ZDOTDIR_ZSHENV: &str = include_str!("../../../../shell-integration/zdotdir/.zshenv");
const ZDOTDIR_ZSHRC: &str = include_str!("../../../../shell-integration/zdotdir/.zshrc");
const ZDOTDIR_ZPROFILE: &str = include_str!("../../../../shell-integration/zdotdir/.zprofile");
const ZDOTDIR_ZLOGIN: &str = include_str!("../../../../shell-integration/zdotdir/.zlogin");

/// Chain bashrc: sources the user's ~/.bashrc (if present) and then our
/// arcterm.bash hooks. Used with `bash --rcfile <this>` on PTY spawn.
/// Not a file on disk in the workspace — we generate it here so the path
/// embedded in the sourced string (`ARCTERM_INTEGRATION_DIR`) is correct
/// per user's $HOME. Kept inline for clarity.
fn bash_rcfile_contents() -> String {
    r#"# ArcTerm bash rcfile — auto-managed, do not edit.
# Chain-loads the user's own ~/.bashrc first so their env, aliases, and
# prompt frameworks all run normally, then sources arcterm.bash last so
# our prompt suppression + shell integration hooks win.

# SECURITY: capture the per-session OSC nonce into a shell-local variable
# and immediately unset the env var before user .bashrc runs, so child
# processes spawned from user rc don't inherit it. arcterm.bash reads
# $__arcterm_osc_nonce to stamp OSC 133/1337 emissions. `declare` without
# `-x` keeps the variable unexported.
declare __arcterm_osc_nonce="${ARCTERM_OSC_NONCE-}"
unset ARCTERM_OSC_NONCE

[[ -r "${HOME}/.bashrc" ]] && source "${HOME}/.bashrc"

: "${ARCTERM_INTEGRATION_DIR:=${HOME}/.arcterm/shell-integration}"
if [[ -r "${ARCTERM_INTEGRATION_DIR}/arcterm.bash" ]]; then
    source "${ARCTERM_INTEGRATION_DIR}/arcterm.bash"
fi
"#.to_string()
}

/// Paths the PTY spawner needs. Returned so we can set env vars without
/// re-computing them.
pub struct Paths {
    /// Value for `ZDOTDIR`. zsh reads `.zshenv`, `.zshrc` etc. from here.
    pub zdotdir: PathBuf,
    /// Value for `ARCTERM_INTEGRATION_DIR`. Scripts use this to locate
    /// their sibling hook file so users can't accidentally break sourcing
    /// by moving files around.
    pub integration_dir: PathBuf,
    /// Path to the bash rcfile we pass via `bash --rcfile <path>`. It
    /// chain-loads the user's own .bashrc then sources arcterm.bash.
    pub bash_rcfile: PathBuf,
    /// Path to the fish hook script. For fish we spawn
    /// `fish -C "source <this>"` rather than using an rcfile since fish
    /// always reads its own config.fish.
    pub fish_hook: PathBuf,
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

    // SECURITY FIX: restrict the arcterm state dir and its children to the
    // current user (0700). Every PTY sources arcterm.zsh from disk; a
    // group/world-writable path would let another local user inject code
    // that runs in the user's shell on next session.
    restrict_dir(&root);
    restrict_dir(&integration_dir);
    restrict_dir(&zdotdir);

    // zsh
    write_file(&integration_dir.join("arcterm.zsh"), ARCTERM_ZSH)?;
    write_file(&zdotdir.join(".zshenv"), ZDOTDIR_ZSHENV)?;
    write_file(&zdotdir.join(".zshrc"), ZDOTDIR_ZSHRC)?;
    // .zprofile + .zlogin are only consumed when zsh runs as a login
    // shell. We spawn zsh with -l specifically so that PATH-setting
    // lines in the user's ~/.zprofile (e.g. `eval "$(brew shellenv)"`)
    // run before .zshrc tries to reference brew. Without this, apps
    // launched from Finder (which get launchd's minimal PATH) fail
    // to find brew in .zshrc.
    write_file(&zdotdir.join(".zprofile"), ZDOTDIR_ZPROFILE)?;
    write_file(&zdotdir.join(".zlogin"), ZDOTDIR_ZLOGIN)?;

    // bash
    write_file(&integration_dir.join("arcterm.bash"), ARCTERM_BASH)?;
    let bash_rcfile = root.join("bash-rcfile");
    write_file(&bash_rcfile, &bash_rcfile_contents())?;

    // fish
    write_file(&integration_dir.join("arcterm.fish"), ARCTERM_FISH)?;
    let fish_hook = integration_dir.join("arcterm.fish");

    log::info!(
        "shell integration installed: zdotdir={} integration={}",
        zdotdir.display(),
        integration_dir.display()
    );

    Ok(Paths {
        zdotdir,
        integration_dir,
        bash_rcfile,
        fish_hook,
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    // Write atomically-ish: we don't bother with a temp-rename dance because
    // a partial write here would only affect the next shell spawn, and we
    // rewrite on every launch anyway.
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    // SECURITY FIX: shell scripts that run in every PTY must be owner-only.
    restrict_file(path);
    Ok(())
}

/// Best-effort chmod 0700. Only meaningful on Unix; elsewhere a no-op.
/// We intentionally ignore the error: if chmod fails the worst case is a
/// more-permissive dir than intended — the fs ops above already succeeded
/// and refusing to boot over a permission-tightening step would be worse UX.
#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}

/// Best-effort chmod 0600 for sensitive files (config.json, shell scripts).
#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}
