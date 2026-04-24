//! Streaming model downloader with progress events and SHA-256 verify.
//!
//! Download flow:
//!   1. Pick up the ModelSpec from the registry.
//!   2. Create ~/.arcterm/models/ if needed.
//!   3. Stream GET the URL; write bytes to a `.part` sibling of the
//!      final filename so a crash doesn't leave a half-download that
//!      looks legitimate.
//!   4. Emit a `model://progress` Tauri event every ~64 KB with
//!      { id, bytes_downloaded, bytes_total }.
//!   5. On complete, SHA-256 verify (if the registry has a hash pinned)
//!      then atomic rename .part -> final name.
//!   6. Emit a `model://done` event with success/error status.
//!
//! Concurrency: one download at a time. The UI's /arcterm-download
//! command rejects a second request while one is active. This keeps
//! bandwidth predictable and avoids surprising Finder when it sees two
//! multi-gigabyte files materializing side-by-side.

use std::path::Path;

use futures::StreamExt;
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::ModelSpec;

pub const EVENT_PROGRESS: &str = "model://progress";
pub const EVENT_DONE: &str = "model://done";

#[derive(Clone, Serialize)]
pub struct ProgressPayload {
    pub id: String,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
}

#[derive(Clone, Serialize)]
pub struct DonePayload {
    pub id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

/// Simple single-slot mutex: only one download may be active at a time.
/// Attempting a concurrent start returns an error rather than queueing.
#[derive(Default)]
pub struct DownloadLock {
    active: Mutex<Option<String>>, // id of the in-flight download
}

impl DownloadLock {
    pub fn try_acquire(&self, id: &str) -> Result<DownloadGuard<'_>, String> {
        let mut slot = self.active.lock();
        if let Some(existing) = slot.as_ref() {
            return Err(format!(
                "A model download ({existing}) is already in progress."
            ));
        }
        *slot = Some(id.to_string());
        Ok(DownloadGuard { lock: self })
    }
}

pub struct DownloadGuard<'a> {
    lock: &'a DownloadLock,
}

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        *self.lock.active.lock() = None;
    }
}

/// Progress-reporting download, emitting events via the supplied AppHandle.
/// Returns the absolute path on success.
///
/// The work is wrapped in tokio::spawn + recoverable catch so that a
/// transport-level panic (reqwest has bubbled panics historically on
/// mid-stream disconnects on macOS) can't propagate up and take the
/// whole Tauri process with it. We convert panics into `Err(...)` so
/// the UI shows a clean error instead of a hard app crash.
pub async fn download(
    app: AppHandle,
    spec: &ModelSpec,
) -> Result<String, String> {
    let spec = spec.clone();
    match tokio::spawn(async move { download_inner(app, spec).await }).await {
        Ok(result) => result,
        Err(join_err) if join_err.is_panic() => {
            Err(format!(
                "download task panicked: {:?}. This sometimes happens on \
                 flaky networks when reqwest's HTTP/2 stream disconnects \
                 abruptly. Retry the download; a resume-friendly implementation \
                 is on the Phase 7 polish list.",
                join_err
            ))
        }
        Err(join_err) => Err(format!("download task failed to join: {join_err}")),
    }
}

/// Body of the download. Split out so we can wrap it in a panic catcher
/// in the public `download()` entry point.
async fn download_inner(
    app: AppHandle,
    spec: ModelSpec,
) -> Result<String, String> {
    // SECURITY FIX: refuse non-HTTPS model URLs. Registry is compile-time-static
    // today, but a future remote-registry feature must not let `http://` or
    // `file://` slip through and stream bytes into the inference engine.
    if !spec.url.starts_with("https://") {
        return Err(format!(
            "refusing to download {}: URL scheme must be https",
            spec.url
        ));
    }

    // SECURITY FIX: refuse to download any model whose registry entry is
    // missing a SHA256. Previously an empty `sha256` silently disabled
    // verify() — a user could load an unverified multi-GB GGUF into the
    // inference engine just because someone forgot to paste the hash. The
    // `every_registry_entry_has_a_sha256` unit test catches missing hashes
    // at build time; this is the runtime backstop in case a future remote
    // registry feeds us an entry we didn't compile in.
    if spec.sha256.is_empty() {
        return Err(format!(
            "refusing to download {}: no SHA256 pinned for model '{}'. \
             Fetch from `curl -sI <url> | grep x-linked-etag` and add to \
             the ModelSpec registry before retrying.",
            spec.url, spec.id
        ));
    }
    if spec.sha256.len() != 64 || !spec.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "refusing to download {}: malformed SHA256 '{}' (must be 64 hex chars)",
            spec.url, spec.sha256
        ));
    }

    let local_path = spec
        .local_path()
        .ok_or_else(|| "HOME not set".to_string())?;
    let dir = local_path
        .parent()
        .ok_or_else(|| "model path has no parent".to_string())?;
    // SECURITY FIX (L-3): create directory with explicit 0700 mode rather
    // than relying on umask-default + post-hoc chmod. `create_dir_all`
    // inherits the process umask (typically 022 → 0755) and leaves a
    // race window between dir creation and `set_permissions`. Pass the
    // mode directly to `mkdir(2)` via `DirBuilderExt::mode` to close it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        builder.mode(0o700);
        builder
            .create(dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    #[cfg(not(unix))]
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("create {}: {e}", dir.display()))?;

    // .part sibling. Written during transfer, renamed on success.
    let part_path = local_path.with_extension(format!(
        "{}.part",
        local_path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));

    // SECURITY FIX (H-1): refuse to proceed if `.part` OR final path
    // exists as a symlink. A same-uid attacker who plants either as a
    // link to e.g. `~/.zshrc` / `~/.ssh/authorized_keys` could use the
    // open-for-append path to corrupt that file with model bytes, or
    // the truncating `File::create` fallback to zero it out. Detect &
    // bail early; user cleans up manually.
    for candidate in [&part_path, &local_path] {
        match std::fs::symlink_metadata(candidate) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to download: {} is a symlink. \
                     Delete it manually before retrying.",
                    candidate.display()
                ));
            }
            _ => {}
        }
    }

    // Resume support: if a .part already exists from a previous attempt,
    // try to continue from where we left off via an HTTP Range request.
    // Saves the user hours of re-download on a flaky connection. We
    // re-hash the existing bytes so the final SHA256 verification still
    // covers the full file; reads sequentially from disk, typically
    // 200-500 MB/s on an SSD so even a 7 GB .part rehashes in under 30s.
    //
    // Use symlink_metadata (not metadata) so this check doesn't silently
    // follow a symlink-back-dated attacker plant.
    let resume_from: u64 = tokio::fs::symlink_metadata(&part_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    if resume_from > 0 {
        log::info!(
            "download resume: rehashing {} bytes of {}",
            resume_from,
            part_path.display()
        );
        // SECURITY FIX (H-1): open the existing .part for rehash with
        // O_NOFOLLOW so we don't hash an attacker's link target instead
        // of the bytes we think are on disk.
        use std::os::unix::fs::OpenOptionsExt;
        let existing_std = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&part_path)
            .map_err(|e| format!("open existing part {}: {e}", part_path.display()))?;
        let mut existing = tokio::fs::File::from_std(existing_std);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = existing
                .read(&mut buf)
                .await
                .map_err(|e| format!("rehash read: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }

    // SECURITY FIX (M-13): pin the client to HTTPS-only and reject
    // cross-scheme redirects. Default `Policy::limited(10)` follows
    // HTTPS→HTTP downgrades; a MITM at the DNS/CDN layer can issue
    // `301 Location: http://evil/…` and turn the download into a
    // plaintext channel. SHA check still catches content tampering,
    // but plaintext leaks which model variant / bytes flow.
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("redirect to non-https rejected")
            } else if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }))
        // A model download can easily take 10 minutes on a slow link;
        // keep the outer timeout generous. Per-read timeouts are handled
        // inside reqwest's stream.
        .timeout(std::time::Duration::from_secs(30 * 60))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    // Range header when resuming. RFC 7233: `bytes=<start>-` (no end
    // means "to the end of the resource"). Servers that support ranges
    // respond with 206 Partial Content + Content-Range; servers that
    // don't respond with 200 OK and the full body, which we detect
    // below and restart-from-zero cleanly.
    let mut req = client.get(spec.url);
    if resume_from > 0 {
        req = req.header("Range", format!("bytes={}-", resume_from));
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("GET {}: {e}", spec.url))?;
    let status = resp.status();

    // SECURITY FIX (H-1): open the `.part` file through a std OpenOptions
    // with `O_NOFOLLOW | O_CLOEXEC` so a swapped-in symlink (planted
    // between the pre-flight check and this open) fails with `ELOOP`
    // instead of letting us truncate/append to an attacker-chosen
    // target. tokio::fs doesn't expose `custom_flags`, so we open
    // synchronously and wrap in `tokio::fs::File::from_std` — the
    // synchronous open is fast and we do it exactly once per download.
    use std::os::unix::fs::OpenOptionsExt;
    let (mut file, mut downloaded, bytes_total) = if status.as_u16() == 206 {
        // Server honored our Range — open the .part for append, continue.
        let total = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total)
            .unwrap_or(spec.size_bytes);
        let f = std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&part_path)
            .map_err(|e| format!("open for append {}: {e}", part_path.display()))?;
        log::info!(
            "download resume accepted: {} / {} bytes already on disk",
            resume_from,
            total
        );
        (tokio::fs::File::from_std(f), resume_from, total)
    } else if status.is_success() {
        // 200 OK despite our Range header → server ignored us (or we
        // had no .part to begin with). Treat as a fresh download:
        // truncate the .part, reset hasher, start at zero.
        if resume_from > 0 {
            log::warn!(
                "server ignored Range header; restarting download from zero"
            );
            hasher = Sha256::new();
        }
        let total = resp.content_length().unwrap_or(spec.size_bytes);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .mode(0o600)
            .open(&part_path)
            .map_err(|e| format!("create {}: {e}", part_path.display()))?;
        (tokio::fs::File::from_std(f), 0u64, total)
    } else {
        return Err(format!(
            "download failed: {} returned {}",
            spec.url, status
        ));
    };

    let mut stream = resp.bytes_stream();
    // Throttle progress events: the stream yields small chunks and we
    // don't want to flood the IPC bus with hundreds of events per second.
    let mut last_emit: u64 = 0;
    const EMIT_EVERY: u64 = 512 * 1024; // every 512 KB
    // SECURITY FIX: hard ceiling on downloaded bytes regardless of what
    // Content-Length claimed. A lying CDN/MITM can't fill the disk by
    // serving more bytes than advertised. 32 GB is larger than any model
    // we'd realistically ship but bounded enough to matter.
    const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("download stream: {e}"))?;
        if downloaded + bytes.len() as u64 > MAX_TOTAL_BYTES {
            return Err(format!(
                "download exceeded max size {} bytes — aborting",
                MAX_TOTAL_BYTES
            ));
        }
        hasher.update(&bytes);
        file.write_all(&bytes)
            .await
            .map_err(|e| format!("write {}: {e}", part_path.display()))?;
        downloaded += bytes.len() as u64;

        if downloaded - last_emit >= EMIT_EVERY || downloaded == bytes_total {
            last_emit = downloaded;
            let _ = app.emit(
                EVENT_PROGRESS,
                ProgressPayload {
                    id: spec.id.to_string(),
                    bytes_downloaded: downloaded,
                    bytes_total,
                },
            );
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("flush {}: {e}", part_path.display()))?;
    drop(file);

    // SHA-256 verify. The empty-hash escape hatch is gone — download_inner
    // refuses entry above if `spec.sha256` is missing or malformed, so by
    // the time we reach this point we are guaranteed a 64-hex-char hash
    // to compare against.
    let digest = hex::encode(hasher.finalize());
    if !digest.eq_ignore_ascii_case(spec.sha256) {
        // Keep the .part around for debugging instead of cleaning up —
        // the user can inspect it if they really want to.
        return Err(format!(
            "SHA-256 mismatch: expected {}, got {}. File preserved at {}",
            spec.sha256,
            digest,
            part_path.display()
        ));
    }

    // SECURITY FIX (H-2): re-check that the rename destination isn't a
    // symlink at the moment of rename. `rename(2)` on macOS will happily
    // overwrite a file at the destination including following it when
    // it's a symlink, which would expose a targeted-overwrite primitive.
    // `symlink_metadata` immediately before rename is racy in principle
    // but tightens the window dramatically — we already refused at
    // download start and no download path creates a symlink under
    // `~/.arcterm/models/`.
    match std::fs::symlink_metadata(&local_path) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!(
                "refusing to rename into {}: destination is a symlink",
                local_path.display()
            ));
        }
        _ => {}
    }

    // Atomic rename into place. At this point the file is fully written,
    // verified (if possible), and ready to be loaded by the inference
    // engine.
    tokio::fs::rename(&part_path, &local_path)
        .await
        .map_err(|e| format!("rename {}: {e}", local_path.display()))?;

    // SECURITY FIX (H-2): set 0600 via fchmod on a fd opened with
    // O_NOFOLLOW, rather than `set_permissions` on a path (chmod(2),
    // which follows symlinks). Avoids the chmod-strip-primitive a
    // same-uid attacker could craft by replacing the destination with
    // a symlink between the rename and the chmod.
    //
    // `File::set_permissions` on Unix is fchmod on the underlying fd,
    // so once we hold the fd from an O_NOFOLLOW open the permission
    // change cannot reach any other file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&local_path)
        {
            Ok(fd) => {
                let _ = fd.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            Err(e) => {
                log::warn!(
                    "post-rename 0600 chmod skipped for {}: {e}",
                    local_path.display()
                );
            }
        }
    }

    Ok(local_path.to_string_lossy().into_owned())
}

/// Remove a downloaded model file. No-op if not installed.
pub async fn uninstall(spec: &ModelSpec) -> Result<(), String> {
    let Some(path) = spec.local_path() else { return Ok(()); };
    if !path.exists() {
        return Ok(());
    }
    tokio::fs::remove_file(&path)
        .await
        .map_err(|e| format!("remove {}: {e}", path.display()))
}

/// Tiny hex helper — avoids pulling in the `hex` crate. Safe for any &[u8].
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let mut s = String::with_capacity(bytes.as_ref().len() * 2);
        for b in bytes.as_ref() {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }
}

/// Parse the total-size portion of a `Content-Range` header.
/// Format: `bytes <start>-<end>/<total>` (or `bytes <start>-<end>/*`
/// when total is unknown). Returns None if we can't parse.
fn parse_content_range_total(header: &str) -> Option<u64> {
    // Example: "bytes 4500000-7999999/8000000"
    let total_part = header.rsplit_once('/').map(|(_, r)| r)?;
    total_part.parse().ok()
}

// Compile-time sanity check: our Path import stays in scope.
#[allow(dead_code)]
fn _assert_path_available(_: &Path) {}
