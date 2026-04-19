//! User settings, persisted to `~/.arcterm/config.json`.
//!
//! Kept deliberately minimal in Phase 5b: just the fields we need to
//! drive the AI backend selection. More fields (shell, font, theme,
//! keybindings) come in Phase 7 when the full settings UI lands. The
//! on-disk format uses camelCase keys because that's what the spec's
//! example config.json shows and it matches how the frontend will read
//! it if we expose settings directly over IPC in the future.
//!
//! Load / save semantics:
//!   - Load: if the file exists, read + parse; otherwise return defaults.
//!     A malformed file falls back to defaults AND logs a warning rather
//!     than failing app startup — we'd rather boot with blank settings
//!     than refuse to launch because of a stray comma.
//!   - Save: atomic write via tempfile + rename so a crash mid-write
//!     can't leave a half-written config that fails to parse next boot.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// The persisted settings shape. Every field is optional in the JSON (via
/// serde defaults) so older or partial configs still load. When we add
/// fields later, old configs read their values as defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// AI-related config. Nested so `ai.mode`, `ai.model`, etc. have
    /// their own dotted namespace when we eventually dump it all as a
    /// settings tree to a UI.
    #[serde(default)]
    pub ai: AiSettings,
    /// UI theme. "dark" (default) or "light". Phase 7+ may add "system"
    /// that tracks the OS appearance. Stored at the top level rather
    /// than nested because it's user-facing and expected to show up
    /// in the settings tree as a top-level toggle.
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ai: AiSettings::default(),
            theme: default_theme(),
        }
    }
}

fn default_theme() -> String {
    "dark".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    /// Backend mode: which backend answers AI requests.
    /// - "claude"  → Claude CLI only (legacy Phase 5a behavior)
    /// - "local"   → local Gemma only
    /// - "auto"    → try Claude, fall back to local on failure
    ///
    /// Default is "auto" so the user's Pro/Max subscription is used when
    /// available and they get a working answer even offline.
    #[serde(default = "default_mode")]
    pub mode: String,

    /// Which local model to load. Keyed to the registry in models.rs.
    /// Default "gemma-4-e2b-it-q4km" is the sensible size/quality
    /// compromise for on-device.
    #[serde(default = "default_local_model")]
    pub local_model: String,

    /// Path override for the `claude` CLI. Empty = PATH lookup.
    #[serde(default)]
    pub claude_path: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            local_model: default_local_model(),
            claude_path: String::new(),
        }
    }
}

fn default_mode() -> String {
    "auto".to_string()
}

fn default_local_model() -> String {
    "gemma-4-e2b-it-q4km".to_string()
}

/// Shared, live settings. Cloneable (Arc under the hood via RwLock<Inner>)
/// so command handlers can read/write without copying the whole struct.
pub struct SettingsStore {
    inner: RwLock<Settings>,
    path: PathBuf,
}

impl SettingsStore {
    /// Open (or create) the settings file at `~/.arcterm/config.json`.
    pub fn open() -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME not set".to_string())?;
        let dir = home.join(".arcterm");
        fs::create_dir_all(&dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;
        // SECURITY FIX: mirror the 0700 mode enforced by shell_hooks in case
        // settings init happens to land first.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        let path = dir.join("config.json");

        let settings = match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Settings>(&contents) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!(
                        "settings parse failed ({}), using defaults — file preserved",
                        e
                    );
                    Settings::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Settings::default()
            }
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };

        Ok(Self {
            inner: RwLock::new(settings),
            path,
        })
    }

    /// SECURITY FIX: fallback constructor for when HOME/config.json can't
    /// be accessed. Returns a store whose `write_to_disk` will still attempt
    /// (and silently fail if the path is unwritable) so we never panic boot.
    pub fn ephemeral() -> Self {
        Self {
            inner: RwLock::new(Settings::default()),
            path: PathBuf::from("/dev/null"),
        }
    }

    pub fn get(&self) -> Settings {
        self.inner.read().clone()
    }

    /// Replace the settings and persist. Callers use this for both full
    /// replace (settings-panel form submit) and partial update (slash
    /// command tweaking one field) — the latter reads, mutates, writes
    /// through this one entry point.
    pub fn set(&self, next: Settings) -> Result<(), String> {
        *self.inner.write() = next.clone();
        self.write_to_disk(&next)
    }

    /// Convenience: mutate in place, then save. The closure gets a &mut
    /// to avoid clone cost for a field-level update.
    pub fn update<F: FnOnce(&mut Settings)>(&self, f: F) -> Result<(), String> {
        let mut guard = self.inner.write();
        f(&mut guard);
        let snapshot = guard.clone();
        drop(guard);
        self.write_to_disk(&snapshot)
    }

    fn write_to_disk(&self, snapshot: &Settings) -> Result<(), String> {
        let serialized = serde_json::to_string_pretty(snapshot)
            .map_err(|e| format!("settings serialize: {e}"))?;
        atomic_write(&self.path, serialized.as_bytes())
            .map_err(|e| format!("settings write {}: {e}", self.path.display()))
    }
}

/// Write file atomically: tmp file in the same dir, fsync, rename.
/// Without this, a crash during write could leave a truncated JSON that
/// fails to parse on next boot. Same-dir tmp ensures rename is atomic on
/// the same filesystem (cross-fs rename falls back to copy + delete).
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;
    let mut tmp = dir.join(
        path.file_name()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no filename"))?,
    );
    tmp.as_mut_os_string().push(".tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    // SECURITY FIX: config.json may hold a custom claudePath that controls
    // which binary AI requests invoke. Tighten to owner-only BEFORE rename
    // so there's never a window where the final file is world-readable.
    restrict_file(&tmp);
    fs::rename(&tmp, path)?;
    restrict_file(path);
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

/// SECURITY FIX: validate `ai.claudePath` before it reaches
/// `ClaudeCliBackend::set_binary`. Without validation, a compromised
/// renderer — or a socially-engineered user — could set this to any
/// file on disk, and the next AI request would spawn that binary as a
/// subprocess with the user's full privileges (env only partly scrubbed
/// to strip Anthropic auth vars).
///
/// Empty string = PATH lookup, always allowed.
///
/// Non-empty requires ALL of:
///   1. Absolute path (no relative paths sneaking in via CWD).
///   2. Exists as a regular file — symlinks rejected because a symlink
///      target can be swapped between this check and actual spawn.
///   3. Owned by the current uid (a binary planted by another user in
///      a shared path never gets executed under our uid).
///   4. Not group- or world-writable (prevents drop-in replacement by
///      any process sharing a less-privileged group with the user).
///   5. Executable bit set for the owner.
///
/// On violation returns a human-readable error. The caller clears the
/// field rather than persisting a poisonous value.
pub fn validate_claude_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Ok(());
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!(
            "claudePath must be an absolute path (got '{path}')"
        ));
    }
    let meta = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) => return Err(format!("claudePath '{path}' not accessible: {e}")),
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "claudePath '{path}' is a symlink; point to the real binary directly"
        ));
    }
    if !meta.is_file() {
        return Err(format!("claudePath '{path}' is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let uid = unsafe { libc_geteuid() };
        if meta.uid() != uid {
            return Err(format!(
                "claudePath '{path}' not owned by current user (uid {uid}); \
                 refusing to execute"
            ));
        }
        let mode = meta.permissions().mode();
        if mode & 0o022 != 0 {
            return Err(format!(
                "claudePath '{path}' is group- or world-writable (mode {:o}); \
                 refusing to execute",
                mode & 0o777
            ));
        }
        if mode & 0o100 == 0 {
            return Err(format!(
                "claudePath '{path}' is not executable by owner (mode {:o})",
                mode & 0o777
            ));
        }
    }
    Ok(())
}

// Direct FFI into geteuid. We avoid importing a whole libc crate — it's
// in the dep tree transitively but not directly, and this is the only
// call site.
#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}
#[cfg(not(unix))]
unsafe fn libc_geteuid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::validate_claude_path;

    #[test]
    fn empty_path_allowed() {
        assert!(validate_claude_path("").is_ok());
        assert!(validate_claude_path("   ").is_ok());
    }

    #[test]
    fn relative_path_rejected() {
        assert!(validate_claude_path("claude").is_err());
        assert!(validate_claude_path("./claude").is_err());
        assert!(validate_claude_path("../claude").is_err());
    }

    #[test]
    fn nonexistent_path_rejected() {
        assert!(validate_claude_path("/no/such/binary/claude-xyz-12345").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_rejected() {
        // /tmp exists and is a directory on every unix; good negative test.
        assert!(validate_claude_path("/tmp").is_err());
    }
}
