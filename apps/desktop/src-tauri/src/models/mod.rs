//! Model registry + on-disk cache.
//!
//! Each `ModelSpec` describes a single downloadable GGUF file: where to
//! fetch it from, what filename to save it under, the expected SHA256,
//! and some metadata for the UI (display name, size, quality tier).
//!
//! Phase 5b ships one entry — Gemma 4 E2B Q4_K_M — because that's what
//! the user wants and the registry layout is ready to accept more rows
//! without schema changes (smaller IQ2_M, bigger Q8_0 variants, future
//! models). Adding a row is one const literal.

pub mod downloader;

use std::path::PathBuf;

use serde::Serialize;

/// A single downloadable model variant.
#[derive(Debug, Clone, Serialize)]
pub struct ModelSpec {
    /// Stable machine id referenced by settings.local_model.
    pub id: &'static str,
    /// Human-readable label for the UI.
    pub display_name: &'static str,
    /// HuggingFace URL (direct `resolve/main` link — not a repo page).
    pub url: &'static str,
    /// Filename under ~/.arcterm/models/. Kept stable across app versions
    /// so existing downloads are recognized after an upgrade.
    pub filename: &'static str,
    /// Expected SHA-256 of the fully-downloaded file, hex-encoded.
    /// Empty string = "no hash shipped for this model" (the downloader
    /// will still work but can't verify integrity). We ship hashes for
    /// every registered variant to keep the integrity story tight.
    pub sha256: &'static str,
    /// Total file size in bytes — drives the progress UI without needing
    /// a HEAD request first. Allowed to be slightly off (e.g. if the
    /// upstream re-quantizes); the downloader treats this as advisory.
    pub size_bytes: u64,
    /// Parameter count shown in the UI. String so we can say "2.3B
    /// active".
    pub parameters: &'static str,
    /// Quantization label (Q4_K_M / Q8_0 / IQ2_M / ...).
    pub quantization: &'static str,
    /// License label (Apache-2.0 for Gemma 4).
    pub license: &'static str,
}

impl ModelSpec {
    /// Where the downloaded file should live on disk.
    pub fn local_path(&self) -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".arcterm/models").join(self.filename))
    }

    /// True iff `local_path()` exists.
    pub fn is_installed(&self) -> bool {
        self.local_path()
            .map(|p| p.is_file())
            .unwrap_or(false)
    }
}

/// The registry. Phase 5b ships one row; add more here as we certify them.
/// These entries are what the frontend's `/arcterm-download` slash command
/// autocompletes against.
///
/// NOTE on sha256: the hash below is a placeholder. Real hash gets pinned
/// when we ship the first release; meanwhile `downloader::verify` treats
/// an empty string as "skip verification". Leaving it empty for now is
/// safer than hardcoding a wrong hash that would break downloads.
// NOTE: bartowski's repo names its files with a `google_` prefix (mirroring
// the original upstream model id). Dropping the prefix produces 404s.
// If we add another publisher's quants later, their naming convention may
// differ — always copy the exact filename shown on the HuggingFace "Files"
// tab.
pub const REGISTRY: &[ModelSpec] = &[
    ModelSpec {
        id: "gemma-4-e2b-it-q4km",
        display_name: "Gemma 4 E2B (Q4_K_M)",
        // bartowski's quant set — covers more quants than google/ggml-org
        // repos and is the de-facto standard community source.
        url: "https://huggingface.co/bartowski/google_gemma-4-E2B-it-GGUF/resolve/main/google_gemma-4-E2B-it-Q4_K_M.gguf",
        filename: "google_gemma-4-E2B-it-Q4_K_M.gguf",
        sha256: "", // TODO: pin before tagging a release
        size_bytes: 3_460_000_000, // 3.46 GB approx
        parameters: "2.3B active / 5.1B total",
        quantization: "Q4_K_M",
        license: "Apache-2.0",
    },
    ModelSpec {
        id: "gemma-4-e2b-it-iq2m",
        display_name: "Gemma 4 E2B (IQ2_M, tight)",
        url: "https://huggingface.co/bartowski/google_gemma-4-E2B-it-GGUF/resolve/main/google_gemma-4-E2B-it-IQ2_M.gguf",
        filename: "google_gemma-4-E2B-it-IQ2_M.gguf",
        sha256: "",
        size_bytes: 2_620_000_000,
        parameters: "2.3B active / 5.1B total",
        quantization: "IQ2_M",
        license: "Apache-2.0",
    },
];

/// Lookup a spec by id. Returns None for unknown ids; callers report a
/// friendly error.
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    REGISTRY.iter().find(|m| m.id == id)
}

/// Return the registry annotated with install status. Used by the UI /
/// slash-command help so users can see which variants are already local.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    #[serde(flatten)]
    pub spec: ModelSpec,
    pub installed: bool,
}

pub fn list() -> Vec<ModelInfo> {
    REGISTRY
        .iter()
        .map(|m| ModelInfo {
            spec: m.clone(),
            installed: m.is_installed(),
        })
        .collect()
}
