//! PTY lifecycle management.
//!
//! Each `spawn` call creates one pseudo-terminal pair plus one child shell
//! process. We keep the master side of the PTY around so we can:
//!   - write keystrokes to the child (via `master.take_writer()`),
//!   - resize on window changes (via `master.resize`),
//!   - drain the child's output on a dedicated reader thread.
//!
//! The reader thread emits a Tauri event "pty://data" with base64 bytes so
//! the frontend can rebuild a Uint8Array losslessly. Base64 is needed because
//! Tauri's event payloads serialize through JSON, which mangles invalid UTF-8
//! that a shell legitimately produces (e.g. progress bars, raw color bytes).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Event names. Mirrored in apps/desktop/src/terminal.ts — keep in sync.
const EVENT_DATA: &str = "pty://data";
const EVENT_EXIT: &str = "pty://exit";

/// Per-PTY state we need to keep alive between IPC calls.
///
/// Everything wrapped in `Mutex` because the `portable_pty` trait objects
/// aren't `Sync` on their own — Tauri's `State<T>` requires `T: Send + Sync`.
struct PtyEntry {
    /// Master half of the PTY pair. We keep ownership so `resize` works and
    /// so the underlying file descriptor stays open while the child runs.
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    /// Writer to the master. `take_writer` consumes the writer slot once, so
    /// we cache it behind a Mutex for the lifetime of the entry.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Handle to the spawned shell. We hold it so we can `kill()` and `wait()`
    /// rather than relying on file-descriptor close to deliver SIGHUP.
    child: Mutex<Box<dyn portable_pty::Child + Send>>,
}

#[derive(Clone, Serialize)]
struct DataPayload {
    id: String,
    /// Base64-encoded bytes from the PTY master.
    data: String,
}

#[derive(Clone, Serialize)]
struct ExitPayload {
    id: String,
    code: Option<i32>,
}

#[derive(Default)]
pub struct PtyManager {
    // Arc + Mutex so reader threads can hold a reference while IPC handlers
    // mutate the map. Inner Arc<PtyEntry> lets a write/resize call grab a
    // single entry without blocking the whole map.
    entries: Arc<Mutex<HashMap<String, Arc<PtyEntry>>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a shell PTY and start its reader thread. Returns the new id.
    pub fn spawn(&self, app: AppHandle, cols: u16, rows: u16) -> Result<String, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                // Pixel sizes are advisory; xterm.js doesn't need them and
                // most programs ignore them. Zero is a valid "unknown" hint.
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e}"))?;

        // Pick the user's preferred shell. $SHELL is the right answer on
        // login terminals; we fall back to /bin/zsh because that's macOS's
        // default and the spec's primary target.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let mut cmd = CommandBuilder::new(&shell);

        // Start in the user's home so the first prompt is somewhere sane
        // rather than wherever Tauri happened to launch from.
        if let Some(home) = dirs_home() {
            cmd.cwd(home);
        }

        // TERM tells programs (vim, less, htop) what escape sequences they
        // can use. xterm-256color is the broadest safe choice for xterm.js.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn shell failed: {e}"))?;

        // take_writer hands us the only Write handle; subsequent calls would
        // panic, so cache it behind a Mutex for the lifetime of the entry.
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take_writer failed: {e}"))?;

        // Master->reader uses try_clone_reader so the master itself stays
        // owned by PtyEntry (needed for resize and to keep FDs alive).
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone_reader failed: {e}"))?;

        let id = Uuid::new_v4().to_string();
        let entry = Arc::new(PtyEntry {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
        });
        self.entries.lock().insert(id.clone(), entry.clone());

        // Reader thread. Blocking read is fine — one thread per PTY is cheap
        // and avoids the complexity of async I/O on raw file descriptors,
        // which differs across macOS/Linux/Windows.
        let app_for_reader = app.clone();
        let id_for_reader = id.clone();
        let entries_for_reader = self.entries.clone();
        thread::Builder::new()
            .name(format!("pty-reader-{}", &id[..8]))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: child closed master
                        Ok(n) => {
                            let payload = DataPayload {
                                id: id_for_reader.clone(),
                                data: BASE64.encode(&buf[..n]),
                            };
                            if let Err(e) = app_for_reader.emit(EVENT_DATA, payload) {
                                log::warn!("emit pty data failed: {e}");
                                break;
                            }
                        }
                        Err(e) => {
                            // EIO on macOS is the normal "child exited" signal
                            // for a PTY master read; log at debug, not error.
                            log::debug!("pty read ended for {id_for_reader}: {e}");
                            break;
                        }
                    }
                }

                // Reap the child so it doesn't linger as a zombie. wait()
                // returns the exit status if the child has finished.
                let code = if let Some(entry) = entries_for_reader.lock().get(&id_for_reader) {
                    entry
                        .child
                        .lock()
                        .wait()
                        .ok()
                        .and_then(|status| status.exit_code().try_into().ok())
                } else {
                    None
                };
                let _ = app_for_reader.emit(
                    EVENT_EXIT,
                    ExitPayload {
                        id: id_for_reader.clone(),
                        code,
                    },
                );
                entries_for_reader.lock().remove(&id_for_reader);
            })
            .map_err(|e| format!("spawn reader thread failed: {e}"))?;

        log::info!("spawned pty {id} shell={shell} cols={cols} rows={rows}");
        Ok(id)
    }

    pub fn write(&self, id: &str, data: &str) -> Result<(), String> {
        let entry = self.get(id)?;
        // Bind the guard to a local so the temporary `entry` outlives it.
        // Frontend sends the literal string xterm produced (UTF-8). Bytes go
        // straight to the PTY; the shell handles its own line discipline.
        let mut writer = entry.writer.lock();
        writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("pty write failed: {e}"))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let entry = self.get(id)?;
        let master = entry.master.lock();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("pty resize failed: {e}"))
    }

    pub fn kill(&self, id: &str) -> Result<(), String> {
        // Take the entry out of the map first so the reader thread sees
        // EOF/EIO and exits cleanly when we drop the master below.
        let entry = self.entries.lock().remove(id);
        if let Some(entry) = entry {
            // killer() returns a sendable handle that signals the child;
            // dropping the master afterwards releases the PTY pair.
            let _ = entry.child.lock().kill();
        }
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Arc<PtyEntry>, String> {
        self.entries
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown pty id: {id}"))
    }
}

/// Cross-platform $HOME resolution without pulling in the `dirs` crate.
fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}
