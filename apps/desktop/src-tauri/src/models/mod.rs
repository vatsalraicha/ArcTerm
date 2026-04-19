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

/// The registry. Phase 5b shipped one row; we now ship five Gemma 4
/// variants. These entries are what the frontend's `/arcterm-download`
/// slash command autocompletes against.
///
/// NOTE on sha256: every entry MUST ship with a real 64-hex-char SHA256
/// of the file's contents. The downloader refuses to run against an
/// empty hash (see `downloader::download_inner`). To refresh these after
/// an upstream re-quantization, HEAD the `resolve/main/<filename>` URL
/// and copy the `x-linked-etag` value (HuggingFace's documented file
/// SHA256, also exposed as `lfs.sha256` in `/api/models/<repo>/tree/main`).
// NOTE: bartowski's repo names its files with a `google_` prefix (mirroring
// the original upstream model id). Dropping the prefix produces 404s.
// If we add another publisher's quants later, their naming convention may
// differ — always copy the exact filename shown on the HuggingFace "Files"
// tab.
pub const REGISTRY: &[ModelSpec] = &[
    // --- Gemma 4 E2B (smaller, ~2.3B active) ------------------------------
    // E2B is the tightest Gemma 4. Good for laptops with 8 GB unified RAM
    // or users who want fast load. Quality is visibly behind E4B on
    // multi-step tool use (as we found in Phase 5b testing).
    ModelSpec {
        id: "gemma-4-e2b-it-q4km",
        display_name: "Gemma 4 E2B (Q4_K_M)",
        url: "https://huggingface.co/bartowski/google_gemma-4-E2B-it-GGUF/resolve/main/google_gemma-4-E2B-it-Q4_K_M.gguf",
        filename: "google_gemma-4-E2B-it-Q4_K_M.gguf",
        sha256: "cded614c9b24be92e5a868d2ba38fb24e15dfea34fc650193c475a6debc233a7",
        size_bytes: 3_462_677_760,
        parameters: "2.3B active / 5.1B total",
        quantization: "Q4_K_M",
        license: "Apache-2.0",
    },
    ModelSpec {
        id: "gemma-4-e2b-it-iq2m",
        display_name: "Gemma 4 E2B (IQ2_M, tight)",
        url: "https://huggingface.co/bartowski/google_gemma-4-E2B-it-GGUF/resolve/main/google_gemma-4-E2B-it-IQ2_M.gguf",
        filename: "google_gemma-4-E2B-it-IQ2_M.gguf",
        sha256: "17e869ac54d0e59faa884d5319fc55ad84cd866f50f0b3073fbb25accc875a23",
        size_bytes: 2_620_198_144,
        parameters: "2.3B active / 5.1B total",
        quantization: "IQ2_M",
        license: "Apache-2.0",
    },

    // --- Gemma 4 E4B (bigger, ~4B active) ---------------------------------
    // E4B reasons noticeably better than E2B and is what we recommend for
    // real tool-use. Needs ~6 GB free RAM during inference on top of the
    // on-disk size. `q4km` is the balanced choice; `iq2m` trades quality
    // for a smaller footprint when disk space is tight; `q8` is the
    // highest-fidelity quant worth shipping (BF16 is too big).
    ModelSpec {
        id: "gemma-4-e4b-it-q4km",
        display_name: "Gemma 4 E4B (Q4_K_M)",
        url: "https://huggingface.co/bartowski/google_gemma-4-E4B-it-GGUF/resolve/main/google_gemma-4-E4B-it-Q4_K_M.gguf",
        filename: "google_gemma-4-E4B-it-Q4_K_M.gguf",
        sha256: "b937a48e96379116137c50acbe39fd1b46eb101d2df4e560f47f5e2171b6451e",
        size_bytes: 5_405_167_904,
        parameters: "4B active / 7.5B total",
        quantization: "Q4_K_M",
        license: "Apache-2.0",
    },
    ModelSpec {
        id: "gemma-4-e4b-it-iq2m",
        display_name: "Gemma 4 E4B (IQ2_M, tight)",
        url: "https://huggingface.co/bartowski/google_gemma-4-E4B-it-GGUF/resolve/main/google_gemma-4-E4B-it-IQ2_M.gguf",
        filename: "google_gemma-4-E4B-it-IQ2_M.gguf",
        sha256: "68ac85596781c6ae7b64fd7febdcddbae51c74de15396af6803e2fcfa5916bb9",
        size_bytes: 3_959_906_592,
        parameters: "4B active / 7.5B total",
        quantization: "IQ2_M",
        license: "Apache-2.0",
    },
    ModelSpec {
        id: "gemma-4-e4b-it-q8",
        display_name: "Gemma 4 E4B (Q8_0, high quality)",
        url: "https://huggingface.co/bartowski/google_gemma-4-E4B-it-GGUF/resolve/main/google_gemma-4-E4B-it-Q8_0.gguf",
        filename: "google_gemma-4-E4B-it-Q8_0.gguf",
        sha256: "9c536ba17e55f3cf4d45aaa985bea7637f7b9034240b1377aca88d873aa6cb5c",
        size_bytes: 8_031_240_480,
        parameters: "4B active / 7.5B total",
        quantization: "Q8_0",
        license: "Apache-2.0",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registry entry must ship a well-formed SHA256. The downloader
    /// rejects empty hashes at runtime; this test catches a missing hash at
    /// build time so a future `/arcterm-download <new-model>` can never
    /// regress integrity verification because someone forgot to paste the
    /// hash in.
    #[test]
    fn every_registry_entry_has_a_sha256() {
        for spec in REGISTRY {
            assert_eq!(
                spec.sha256.len(),
                64,
                "model '{}' sha256 must be 64 hex chars (HuggingFace x-linked-etag). \
                 Run `curl -sI <url> | grep x-linked-etag` to fetch it.",
                spec.id
            );
            assert!(
                spec.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "model '{}' sha256 contains non-hex character(s)",
                spec.id
            );
        }
    }

    /// IDs are used as the keys in settings.ai.localModel and in the
    /// `/arcterm-download <id>` slash command. Duplicates would silently
    /// shadow one another — catch them early.
    #[test]
    fn registry_ids_are_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for spec in REGISTRY {
            assert!(
                !seen.contains(&spec.id),
                "duplicate model id in REGISTRY: '{}'",
                spec.id
            );
            seen.push(spec.id);
        }
    }
}

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

/// Sweep `~/.arcterm/models/` for stale `.part` files and delete them.
///
/// A `.part` file appears when a download is interrupted mid-transfer
/// (crash, kill -9, laptop closed on cellular). Two distinct situations:
///
///   - **Recent** (< 7 days): probably a download the user wants to
///     resume. The downloader re-opens these, rehashes the existing
///     bytes, and continues via an HTTP Range request.
///   - **Stale** (>= 7 days): the model or URL has likely changed; the
///     bytes on disk are dead weight. Delete.
///
/// Returns (removed, bytes) for a log summary.
pub fn cleanup_stranded_parts() -> (usize, u64) {
    const STALE_AGE_SECS: u64 = 7 * 24 * 60 * 60;
    let Some(home) = std::env::var_os("HOME") else {
        return (0, 0);
    };
    let dir = std::path::PathBuf::from(home).join(".arcterm/models");
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return (0, 0),
    };
    let mut removed = 0usize;
    let mut bytes = 0u64;
    let now = std::time::SystemTime::now();
    for entry in read.flatten() {
        let path = entry.path();
        let is_part = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s == "part")
            .unwrap_or(false);
        if !is_part {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        // Keep the .part if it's young enough to resume from. mtime works
        // for our needs — modified-recently means the download was still
        // writing not long ago.
        if let Ok(mtime) = meta.modified() {
            if let Ok(age) = now.duration_since(mtime) {
                if age.as_secs() < STALE_AGE_SECS {
                    log::info!(
                        "preserving recent partial download for resume: {} ({} MB, {}s old)",
                        path.display(),
                        meta.len() / 1_048_576,
                        age.as_secs()
                    );
                    continue;
                }
            }
        }
        bytes += meta.len();
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
            log::info!("removed stale download: {}", path.display());
        }
    }
    (removed, bytes)
}
