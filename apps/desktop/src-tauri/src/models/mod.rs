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

/// SECURITY FIX: re-verify a GGUF's SHA256 before `LocalLlamaBackend::load`
/// mmap's it.
///
/// The downloader hashes the bytes as they stream in and refuses the
/// atomic rename unless the hash matches the registry pin, so the file
/// is trustworthy *at the moment it's installed*. After that, it's
/// trust-on-first-use forever: any same-uid process can overwrite the
/// file in place, and the next `ai_set_local_model` / `ai_set_mode` /
/// `/arcterm-load` would mmap-parse the attacker's crafted GGUF
/// through llama-cpp-2. Known GGUF parser CVEs exist; we shouldn't
/// trust on-disk bytes just because we once trusted them.
///
/// This function rehashes the full file (reads sequentially off disk,
/// ~15–25 s for a 3 GB model on a modern SSD) and compares against
/// the registry pin. On mismatch, returns a human-readable error and
/// the caller refuses to load.
///
/// Skipped (with a logged warning) when the path doesn't map to a
/// registry entry — we currently only load registry-shipped models,
/// but the future plan to accept user-dropped GGUFs needs a separate
/// trust path (sidecar signatures, explicit opt-in) rather than
/// silently extending this check.
pub fn verify_integrity(path: &std::path::Path) -> Result<(), String> {
    verify_integrity_with_progress(path, |_, _| {})
}

/// Same as `verify_integrity` but invokes `progress(bytes_hashed, total_bytes)`
/// as the hash advances. Used by the Wave 2.5 boot-path loader to stream
/// a percentage up to the toolbar pill. Chunk size (1 MiB) means the
/// callback fires ~8000 times for an 8 GB file — callers that care about
/// event rate should throttle on their own (e.g. only emit on integer
/// percent boundaries).
pub fn verify_integrity_with_progress(
    path: &std::path::Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::fs::File;
    use std::io::Read;

    let basename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("GGUF path has no filename: {}", path.display()))?;
    let Some(spec) = REGISTRY.iter().find(|s| s.filename == basename) else {
        log::warn!(
            "GGUF at {} not in registry; skipping SHA verify. \
             Only registry-shipped models are currently supported for \
             integrity checks.",
            path.display()
        );
        return Ok(());
    };

    // Total size for the progress denominator. Prefer the actual on-disk
    // length over `spec.size_bytes` (advisory) so a tiny-off registry
    // pin doesn't make the bar stop at 99% or overshoot to 101%.
    let total_bytes = std::fs::metadata(path)
        .map(|m| m.len())
        .unwrap_or(spec.size_bytes);

    let f = File::open(path)
        .map_err(|e| format!("open {} for verify: {e}", path.display()))?;
    // Ask the kernel to prefetch the whole file sequentially. On macOS
    // this is F_RDADVISE (Darwin-specific; off-cheap, fire-and-forget).
    // Measurable win on cold reads because the readahead runs in
    // parallel with our SHA-256 loop — disk isn't idle waiting for the
    // hasher to catch up. Silently skipped on non-Unix targets.
    #[cfg(target_os = "macos")]
    darwin_prefetch(&f, total_bytes);
    let mut f = f;
    let mut hasher = Sha256::new();
    // 8 MiB chunks: larger than 1 MiB gives the kernel a better hint for
    // readahead and cuts the per-read syscall count by 8× at negligible
    // memory cost. Anything much larger stops helping on macOS (HFS/APFS
    // tends to cap sequential prefetch around 8–16 MiB).
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    let mut bytes_hashed: u64 = 0;
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {} during verify: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_hashed += n as u64;
        progress(bytes_hashed, total_bytes);
    }
    // Final tick so the caller always gets a 100% emit even if the file
    // grew mid-hash (unlikely — the file is under 0600 — but cheap).
    progress(bytes_hashed, total_bytes);

    let digest = hex_encode(hasher.finalize());
    if !digest.eq_ignore_ascii_case(spec.sha256) {
        return Err(format!(
            "GGUF integrity check failed for {}: expected SHA256 {}, got {}. \
             File may have been tampered with since install. Delete with \
             `/arcterm-models`-driven uninstall and re-download.",
            spec.id, spec.sha256, digest
        ));
    }
    log::info!(
        "GGUF integrity verified: {} matches pinned SHA256",
        spec.id
    );
    Ok(())
}

/// macOS-only: kick off an async kernel readahead for the whole file.
/// Uses `fcntl(fd, F_RDADVISE, &radvisory)` — Darwin's equivalent of
/// `posix_fadvise(POSIX_FADV_WILLNEED)`. Fire-and-forget: the kernel
/// pages-in as it can, our SHA loop consumes pages as they arrive.
///
/// F_RDADVISE value is 44 on Darwin (xnu-based kernels). We open-code
/// the ffi rather than pull in `libc` just for this constant.
#[cfg(target_os = "macos")]
fn darwin_prefetch(f: &std::fs::File, total_bytes: u64) {
    use std::os::unix::io::AsRawFd;

    #[repr(C)]
    struct Radvisory {
        ra_offset: i64,
        ra_count: i32,
    }
    const F_RDADVISE: i32 = 44;

    extern "C" {
        #[link_name = "fcntl"]
        fn fcntl_rdadvise(fd: i32, cmd: i32, arg: *const Radvisory) -> i32;
    }

    let adv = Radvisory {
        ra_offset: 0,
        // ra_count is int32; clamp for files near or past 2 GiB is
        // unnecessary here because we pass a count not an end offset,
        // but keep the saturating_as for safety. Files larger than
        // 2 GiB still benefit — kernel will just prefetch the first
        // 2 GiB async, which is plenty of lead for the hasher.
        ra_count: total_bytes.min(i32::MAX as u64) as i32,
    };
    unsafe {
        let _ = fcntl_rdadvise(f.as_raw_fd(), F_RDADVISE, &adv);
    }
}

/// Local hex encoder so we don't pull in the `hex` crate just for this.
/// Kept module-private — the downloader has its own inline copy; sharing
/// would cross a module boundary for two call sites, not worth it.
fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        s.push_str(&format!("{:02x}", b));
    }
    s
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
        // SECURITY FIX: only preserve `.part` files whose stem maps to a
        // current registry entry. Without this, any `*.part` the user's
        // uid could write to `~/.arcterm/models/` would survive cleanup
        // indefinitely, and a future `/arcterm-download` of a matching
        // filename would treat the attacker-written prefix as resumable
        // state — forcing a rehash of attacker bytes into our hasher
        // before the real stream appends. Hash mismatch still fails the
        // download (downstream defense), but cleanup is the right place
        // to kill the squat. Unrecognized .part files are always deleted.
        let Some(filename) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let expected_filename = filename.trim_end_matches(".part");
        let in_registry = REGISTRY
            .iter()
            .any(|s| s.filename == expected_filename);
        // SECURITY FIX (H-1): use symlink_metadata / check file_type
        // before remove_file. entry.metadata() follows symlinks — a
        // same-uid attacker who plants `<registered>.part` as a symlink
        // to e.g. `~/.zshrc` would cause this sweep (and downstream
        // download code) to touch the victim target. Refuse outright and
        // surface via log; user investigates manually. `unlink(2)`
        // semantics of remove_file *on a symlink* would only remove the
        // link, not the target — but we skip even that to avoid
        // obliterating evidence during forensics.
        let Ok(meta) = entry.path().symlink_metadata() else { continue };
        if meta.file_type().is_symlink() {
            log::warn!(
                "refusing to touch symlinked .part file at {} \
                 (possible tampering; investigate manually)",
                path.display()
            );
            continue;
        }
        if !in_registry {
            bytes += meta.len();
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
                log::warn!(
                    "removed unrecognized .part file (not in registry): {}",
                    path.display()
                );
            }
            continue;
        }
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

/// SECURITY FIX: boot-time sweep to normalize GGUF file permissions to
/// 0600.
///
/// Wave 2 chmods fresh downloads to 0600 after atomic-rename, but any
/// file installed before Wave 2 kept its umask-default (typically
/// 0644, observed in the wild: `-rw-r--r--`). The dir itself is 0700
/// so other local users can't read these files regardless, but 0600
/// is the documented policy and inconsistency is future-drift risk
/// — if the dir perms ever slip (e.g. after a restore from a backup
/// that doesn't preserve modes), the file perms become the only
/// protection against same-uid-but-different-process readers.
///
/// Cheap: one `read_dir` + a `set_permissions` per `.gguf`. Idempotent.
/// Returns (normalized, total_scanned) for a log summary.
#[cfg(unix)]
pub fn normalize_model_perms() -> (usize, usize) {
    use std::os::unix::fs::PermissionsExt;

    let Some(home) = std::env::var_os("HOME") else {
        return (0, 0);
    };
    let dir = std::path::PathBuf::from(home).join(".arcterm/models");
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return (0, 0),
    };
    let mut normalized = 0usize;
    let mut total = 0usize;
    for entry in read.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s != "gguf")
            .unwrap_or(true)
        {
            continue;
        }
        total += 1;
        // SECURITY FIX (H-1): don't follow symlinks when checking / changing
        // mode on registry-installed GGUFs. set_permissions → chmod(2)
        // follows symlinks, which would let a same-uid attacker whose
        // planted link survives this path use us as a chmod-strip primitive.
        let Ok(meta) = entry.path().symlink_metadata() else { continue };
        if meta.file_type().is_symlink() {
            log::warn!(
                "refusing to normalize perms on symlinked GGUF at {} \
                 (possible tampering; investigate manually)",
                path.display()
            );
            continue;
        }
        let current_mode = meta.permissions().mode() & 0o777;
        if current_mode == 0o600 {
            continue;
        }
        // Open with O_NOFOLLOW so fchmod cannot follow a swapped-in
        // symlink between the symlink_metadata check above and this
        // call. `File::set_permissions` on Unix is fchmod on the fd.
        use std::os::unix::fs::OpenOptionsExt;
        let open = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path);
        let Ok(fd) = open else { continue };
        if fd
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .is_ok()
        {
            normalized += 1;
            log::info!(
                "normalized model perms: {} ({:o} -> 600)",
                path.display(),
                current_mode
            );
        }
    }
    (normalized, total)
}

#[cfg(not(unix))]
pub fn normalize_model_perms() -> (usize, usize) {
    (0, 0)
}
