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
use tokio::io::AsyncWriteExt;

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
    let local_path = spec
        .local_path()
        .ok_or_else(|| "HOME not set".to_string())?;
    let dir = local_path
        .parent()
        .ok_or_else(|| "model path has no parent".to_string())?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("create {}: {e}", dir.display()))?;

    // .part sibling. Written during transfer, renamed on success.
    let part_path = local_path.with_extension(format!(
        "{}.part",
        local_path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));

    let client = reqwest::Client::builder()
        // A model download can easily take 10 minutes on a slow link;
        // keep the outer timeout generous. Per-read timeouts are handled
        // inside reqwest's stream.
        .timeout(std::time::Duration::from_secs(30 * 60))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let resp = client
        .get(spec.url)
        .send()
        .await
        .map_err(|e| format!("GET {}: {e}", spec.url))?;
    if !resp.status().is_success() {
        return Err(format!(
            "download failed: {} returned {}",
            spec.url,
            resp.status()
        ));
    }

    let bytes_total = resp
        .content_length()
        .unwrap_or(spec.size_bytes);
    let mut stream = resp.bytes_stream();

    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| format!("create {}: {e}", part_path.display()))?;

    let mut downloaded: u64 = 0;
    let mut hasher = Sha256::new();
    // Throttle progress events: the stream yields small chunks and we
    // don't want to flood the IPC bus with hundreds of events per second.
    let mut last_emit: u64 = 0;
    const EMIT_EVERY: u64 = 512 * 1024; // every 512 KB

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("download stream: {e}"))?;
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

    // SHA-256 verify (only if we have a pinned hash for this model).
    if !spec.sha256.is_empty() {
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
    }

    // Atomic rename into place. At this point the file is fully written,
    // verified (if possible), and ready to be loaded by the inference
    // engine.
    tokio::fs::rename(&part_path, &local_path)
        .await
        .map_err(|e| format!("rename {}: {e}", local_path.display()))?;

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

// Compile-time sanity check: our Path import stays in scope.
#[allow(dead_code)]
fn _assert_path_available(_: &Path) {}
