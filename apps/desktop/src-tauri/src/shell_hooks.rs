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
const ARCTERM_PS1: &str = include_str!("../../../../shell-integration/arcterm.ps1");
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
    /// Path to the PowerShell hook. Informational only — the spawn path
    /// does not reference it (see `powershell_encoded_command`); it is on
    /// disk so users can read what runs in their shell and dot-source it
    /// by hand in a PowerShell that ArcTerm did not start.
    pub powershell_hook: PathBuf,
}

/// Install (or refresh) the shell integration files under `~/.arcterm/`.
///
/// Idempotent and cheap — the files together are a few KB and writing them
/// every launch avoids a whole class of "stale script" bugs.
pub fn install() -> Result<Paths, String> {
    let home = crate::home::dir()
        .ok_or_else(|| format!("{}; cannot install shell integration", crate::home::MISSING))?;

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

    // PowerShell. Written on every platform (pwsh exists on macOS and
    // Linux too, and the file is a few KB); only the Windows spawn path
    // injects it today.
    let powershell_hook = integration_dir.join("arcterm.ps1");
    write_file(&powershell_hook, ARCTERM_PS1)?;

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
        powershell_hook,
    })
}

/// The PowerShell hook packaged for `pwsh -EncodedCommand`.
///
/// WHY NOT DOT-SOURCE THE FILE: PowerShell's execution policy governs
/// *script files*. A stock Windows client ships `Restricted`, which blocks
/// every `.ps1` — ours included — so `-Command ". arcterm.ps1"` would
/// silently hand the user a shell with no blocks, no cwd and no exit
/// codes. Commands given on the command line are exempt from the policy,
/// and `-EncodedCommand` is the one way to pass a multi-line script
/// through `CreateProcess` without a quoting nightmare. The encoding is
/// base64 over UTF-16LE, as `pwsh -?` documents.
///
/// Comment-only and blank lines are dropped first: the script is mostly
/// commentary, the Windows command line tops out at 32 K characters, and
/// the file on disk keeps the prose for anyone who wants to read it.
pub fn powershell_encoded_command() -> String {
    encode_utf16le_base64(&strip_comment_lines(ARCTERM_PS1))
}

fn strip_comment_lines(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    for line in script.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn encode_utf16le_base64(script: &str) -> String {
    use base64::Engine as _;
    let mut bytes = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    // SECURITY FIX (M-7): an attacker (or a previous-install bug) that
    // pre-plants `~/.arcterm/zdotdir/.zshrc` as a symlink to the user's
    // real `~/.zshrc` would otherwise cause `fs::write` to follow and
    // truncate the target — silently wiping the user's shell config
    // on the next ArcTerm launch. Use `O_NOFOLLOW | O_CREAT | O_TRUNC`
    // so a pre-existing symlink causes the open to fail (ELOOP) rather
    // than silently victimize the link target.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        f.write_all(content.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
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


#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn decode_utf16le_base64(encoded: &str) -> String {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(bytes.len() % 2, 0, "UTF-16LE payload must be even-length");
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).expect("valid UTF-16")
    }

    /// `pwsh -EncodedCommand` decodes base64 as UTF-16LE; getting the byte
    /// order or width wrong yields a script of CJK garbage that fails to
    /// parse, and the only symptom would be a Windows user with no shell
    /// integration. Round-trip the exact encoding here.
    #[test]
    fn encoded_command_round_trips_as_utf16le() {
        let script = strip_comment_lines(ARCTERM_PS1);
        assert_eq!(decode_utf16le_base64(&powershell_encoded_command()), script);
    }

    /// Every code line survives stripping; every comment-only line is
    /// gone. Guards against a future edit that puts code after a `#` on
    /// the same line thinking it will be kept — the file header says not
    /// to, this test enforces it.
    #[test]
    fn stripping_keeps_every_code_line_and_no_comment_lines() {
        let stripped = strip_comment_lines(ARCTERM_PS1);
        for line in ARCTERM_PS1.lines() {
            let t = line.trim_start();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            assert!(stripped.contains(line), "code line dropped: {line}");
        }
        assert!(!stripped.lines().any(|l| l.trim_start().starts_with('#')));
        assert!(stripped.contains("function global:prompt"));
        assert!(stripped.contains("__arcterm_osc_nonce"));
    }

    /// The whole thing has to fit in a Windows command line (32 767
    /// characters) alongside `pwsh -NoLogo -NoExit -EncodedCommand`.
    #[test]
    fn encoded_command_fits_a_windows_command_line() {
        assert!(powershell_encoded_command().len() < 24_000);
    }
}
