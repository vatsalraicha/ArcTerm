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
    /// Persisted sidebar sessions. The shape (count + names) the user
    /// had last time they quit. On boot the frontend recreates one
    /// session per entry, in this order, so closing + reopening the app
    /// preserves both how many tabs were open AND any custom names.
    /// Empty vec = first launch / nothing persisted yet → frontend
    /// falls back to its single-default-session behavior.
    #[serde(default)]
    pub sessions: Vec<PersistedSession>,
}

/// Per-session state we persist across restarts. Kept deliberately
/// minimal: PTY ids regenerate every boot, cwd belongs to a live shell
/// process that no longer exists, and exit codes / running flags are
/// transient. Only the user-visible name is stable enough — and useful
/// enough — to restore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSession {
    /// Sidebar label. Truncated to MAX_SESSION_NAME_BYTES on save so a
    /// poisoned config.json can't allocate gigabytes when we reload.
    pub name: String,
}

/// Hard caps on the persisted-sessions list so a malformed or hostile
/// config.json can't blow up boot. 64 is several times any realistic
/// open-tab count; the per-name 256-byte cap leaves room for unicode
/// labels while staying well under any UI sanity bound.
pub const MAX_PERSISTED_SESSIONS: usize = 64;
pub const MAX_SESSION_NAME_BYTES: usize = 256;

/// Clamp a sessions list to the documented caps. Truncates the list
/// length, the per-name byte length (on a UTF-8 char boundary), and
/// drops empty names. Used both at the IPC boundary (so a renderer
/// can't smuggle giant payloads through `sessions_set`) and at boot
/// load (so a hand-edited or hostile config.json can't poison the
/// in-memory copy). Returns a fresh Vec; never mutates input.
pub fn sanitize_sessions(input: &[PersistedSession]) -> Vec<PersistedSession> {
    input
        .iter()
        .filter_map(|s| {
            let name = s.name.trim();
            if name.is_empty() {
                return None;
            }
            // Byte-length cap respecting char boundaries: walk forward
            // and stop at the last boundary ≤ MAX_SESSION_NAME_BYTES.
            let truncated = if name.len() <= MAX_SESSION_NAME_BYTES {
                name.to_string()
            } else {
                let mut end = MAX_SESSION_NAME_BYTES;
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                name[..end].to_string()
            };
            Some(PersistedSession { name: truncated })
        })
        .take(MAX_PERSISTED_SESSIONS)
        .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ai: AiSettings::default(),
            theme: default_theme(),
            sessions: Vec::new(),
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

    /// SECURITY vs. UX escape hatch. When `true` (default), the Wave 2.5
    /// background boot-load pass runs a full SHA-256 re-verify against
    /// the registry pin before llama.cpp mmap's the file — closes the
    /// post-install-tamper-across-reboots window (see GHSA-vgg9-87g3-85w8
    /// and siblings, which fire at `gguf_init_from_file` time).
    ///
    /// Power users with slow disks (user-report: 5 min for an 8 GB GGUF)
    /// can set this to `false` to skip the hash on boot. Trade-off:
    /// the window from last-verified-download through next boot is
    /// unprotected against same-uid on-disk tamper. All user-triggered
    /// swap paths (ai_set_mode, ai_set_local_model, model_download
    /// post-load) STILL verify unconditionally, so the weakening only
    /// covers the restart-without-interacting-with-AI case.
    #[serde(default = "default_verify_on_boot")]
    pub verify_on_boot: bool,
}

fn default_verify_on_boot() -> bool {
    true
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            local_model: default_local_model(),
            claude_path: String::new(),
            verify_on_boot: default_verify_on_boot(),
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

        // SECURITY FIX (L-4): cap config.json reads at 1 MiB. A same-uid
        // attacker (or a buggy write path) that leaves a 500 MB config on
        // disk would otherwise allocate that much memory on every boot.
        // Real settings files are a few hundred bytes; 1 MiB is orders of
        // magnitude above the realistic ceiling.
        const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
        let settings = match fs::File::open(&path) {
            Ok(f) => {
                use std::io::Read;
                let mut contents = String::new();
                let mut take = f.take(MAX_CONFIG_BYTES + 1);
                match take.read_to_string(&mut contents) {
                    Ok(_) => {
                        if contents.len() as u64 > MAX_CONFIG_BYTES {
                            log::warn!(
                                "config.json exceeds {} byte cap; using defaults \
                                 (file preserved for inspection)",
                                MAX_CONFIG_BYTES
                            );
                            Settings::default()
                        } else {
                            serde_json::from_str::<Settings>(&contents)
                                .unwrap_or_else(|e| {
                                    log::warn!(
                                        "settings parse failed ({}), using defaults — \
                                         file preserved",
                                        e
                                    );
                                    Settings::default()
                                })
                        }
                    }
                    Err(e) => return Err(format!("read {}: {e}", path.display())),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(format!("open {}: {e}", path.display())),
        };

        // SECURITY FIX (M-17): re-validate claudePath at boot. Validation
        // only fires on `settings_set`; a pinned path that has since been
        // replaced (brew reinstall, filesystem move), or a config.json
        // flipped by a brief-write-access attacker, would otherwise be
        // trusted at spawn time with no re-check. On failure we clear the
        // in-memory copy and fall back to PATH lookup — but we do NOT
        // clobber disk, so the user can see the old value in the settings
        // panel and decide whether it was intentional.
        let mut final_settings = settings;
        // Re-clamp the persisted sessions list. We trust our own writer to
        // emit sanitized values, but the file could have been hand-edited
        // (or written by a future/older version). Doing the clamp here
        // means the rest of the app can treat `settings.sessions` as
        // already-validated.
        final_settings.sessions = sanitize_sessions(&final_settings.sessions);
        if let Err(e) = validate_claude_path(&final_settings.ai.claude_path) {
            log::warn!(
                "stored claudePath failed boot revalidation ({e}); \
                 clearing in-memory copy, PATH lookup will be used"
            );
            final_settings.ai.claude_path.clear();
        }

        Ok(Self {
            inner: RwLock::new(final_settings),
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
        // SECURITY FIX (M-17): walk every ancestor directory and assert
        // it is owned by the user or root and not group/world-writable.
        // Without this, a binary at `/tmp/mine/claude` passes the file-
        // level checks even if `/tmp/mine/` itself is world-writable —
        // an attacker can then `mv claude claude.bak; cp /bin/sh claude`
        // and win. Mirrors OpenSSH's StrictModes.
        let mut current: Option<&Path> = p.parent();
        while let Some(dir) = current {
            let dir_meta = match fs::symlink_metadata(dir) {
                Ok(m) => m,
                Err(e) => {
                    return Err(format!(
                        "claudePath ancestor '{}' not accessible: {e}",
                        dir.display()
                    ));
                }
            };
            let dir_mode = dir_meta.permissions().mode();
            if dir_mode & 0o022 != 0 {
                return Err(format!(
                    "claudePath ancestor '{}' is group/world-writable (mode {:o}); \
                     refusing to execute",
                    dir.display(),
                    dir_mode & 0o777
                ));
            }
            let dir_uid = dir_meta.uid();
            if dir_uid != uid && dir_uid != 0 {
                return Err(format!(
                    "claudePath ancestor '{}' owned by uid {} (not user or root); \
                     refusing to execute",
                    dir.display(),
                    dir_uid
                ));
            }
            // Stop at filesystem root.
            let next = dir.parent();
            if next == Some(dir) || next.is_none() {
                break;
            }
            current = next;
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
    use super::{
        sanitize_sessions, validate_claude_path, PersistedSession, MAX_PERSISTED_SESSIONS,
        MAX_SESSION_NAME_BYTES,
    };

    fn p(name: &str) -> PersistedSession {
        PersistedSession {
            name: name.to_string(),
        }
    }

    #[test]
    fn sanitize_drops_empty_names() {
        let out = sanitize_sessions(&[p(""), p("   "), p("real")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "real");
    }

    #[test]
    fn sanitize_truncates_list_length() {
        let input: Vec<_> = (0..MAX_PERSISTED_SESSIONS + 5)
            .map(|i| p(&format!("s{i}")))
            .collect();
        let out = sanitize_sessions(&input);
        assert_eq!(out.len(), MAX_PERSISTED_SESSIONS);
    }

    #[test]
    fn sanitize_truncates_long_name_on_char_boundary() {
        // "é" = 2 bytes; cap of 256 bytes means we should keep ≤ 128 chars.
        let huge = "é".repeat(MAX_SESSION_NAME_BYTES);
        let out = sanitize_sessions(&[p(&huge)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].name.len() <= MAX_SESSION_NAME_BYTES);
        // Result must still be valid UTF-8 (Rust enforces this for &str,
        // but truncating mid-codepoint would have panicked).
        assert!(out[0].name.chars().all(|c| c == 'é'));
    }

    #[test]
    fn sanitize_preserves_order() {
        let out = sanitize_sessions(&[p("a"), p("b"), p("c")]);
        let names: Vec<_> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn sanitize_trims_whitespace() {
        let out = sanitize_sessions(&[p("  build server  ")]);
        assert_eq!(out[0].name, "build server");
    }


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
