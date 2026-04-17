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
    fs::rename(&tmp, path)
}
