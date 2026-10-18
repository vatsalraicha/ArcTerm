//! Home-directory resolution shared by every module that touches
//! `~/.arcterm/`.
//!
//! WHY THIS EXISTS: the codebase grew up on macOS, where `$HOME` is always
//! set, so eight separate call sites did `std::env::var_os("HOME")`.
//! Windows never sets `HOME` — its equivalent is `USERPROFILE` — so every
//! one of them failed there in the same way: history, settings, the model
//! cache, shell-hook install, `~` completion and the Claude CLI cwd pin.
//! One resolver means one place to get it right.
//!
//! Precedence: `HOME` first, even on Windows, so a user (or a test) who
//! sets it explicitly gets exactly what they asked for; then `USERPROFILE`
//! on Windows only. Empty values count as unset — sandboxed launchers do
//! export `HOME=` and a bare empty path would otherwise resolve to the cwd.

use std::ffi::OsString;
use std::path::PathBuf;

/// Error text for callers that need a `String` failure.
pub const MISSING: &str = "home directory not found (HOME / USERPROFILE unset)";

/// The user's home directory, or `None` when neither variable is usable.
pub fn dir() -> Option<PathBuf> {
    dir_from(std::env::var_os("HOME"), windows_profile())
}

#[cfg(windows)]
fn windows_profile() -> Option<OsString> {
    std::env::var_os("USERPROFILE")
}

#[cfg(not(windows))]
fn windows_profile() -> Option<OsString> {
    None
}

/// Pure half of `dir()`, split out so the precedence rules are testable
/// without mutating process-global environment variables (which races
/// with cargo's parallel test runner).
fn dir_from(home: Option<OsString>, userprofile: Option<OsString>) -> Option<PathBuf> {
    home.filter(|h| !h.is_empty())
        .or_else(|| userprofile.filter(|p| !p.is_empty()))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn home_wins_when_both_are_set() {
        assert_eq!(
            dir_from(os("/Users/me"), os(r"C:\Users\me")),
            Some(PathBuf::from("/Users/me"))
        );
    }

    #[test]
    fn empty_home_falls_back_to_userprofile() {
        assert_eq!(
            dir_from(os(""), os(r"C:\Users\me")),
            Some(PathBuf::from(r"C:\Users\me"))
        );
    }

    #[test]
    fn missing_home_uses_userprofile() {
        assert_eq!(
            dir_from(None, os(r"C:\Users\me")),
            Some(PathBuf::from(r"C:\Users\me"))
        );
    }

    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(dir_from(None, None), None);
        assert_eq!(dir_from(os(""), os("")), None);
    }
}
