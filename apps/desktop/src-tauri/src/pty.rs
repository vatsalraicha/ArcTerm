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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::shell_hooks;

/// Event names. Mirrored in apps/desktop/src/terminal.ts — keep in sync.
const EVENT_DATA: &str = "pty://data";
const EVENT_EXIT: &str = "pty://exit";
/// Emitted whenever the child transitions into or out of reading a secret.
/// Drives the frontend's secure-input mode — see `read_secret_state`.
const EVENT_SECRET: &str = "pty://secret-input";

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
    /// Last secret-input state we told the frontend about, so the reader
    /// thread can emit only on transitions rather than once per burst.
    ///
    /// Seeded `false`: the normal state is "not reading a secret", so the
    /// first transition we ever report is the interesting one.
    last_secret: AtomicBool,
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

#[derive(Clone, Serialize)]
struct SecretPayload {
    id: String,
    /// `true` means the child is reading a secret and the frontend should
    /// take its custom input UI away.
    secret: bool,
}

/// Return shape of `PtyManager::spawn`. The frontend needs the PTY id
/// (for subsequent IPC calls) and the session's OSC nonce (to validate
/// inbound shell-integration sequences against).
#[derive(Clone, Serialize)]
pub struct SpawnResult {
    pub id: String,
    #[serde(rename = "oscNonce")]
    pub osc_nonce: String,
}

/// Does this termios local-flag word describe a process reading a secret?
///
/// The rule is `ECHO off AND ICANON on`, and both halves are load-bearing:
///
///   - **ECHO off** is how a program hides what you type. `sudo`'s
///     `tgetpass()`, ssh, gpg and `passwd` all clear it before reading.
///   - **ICANON on** is what separates a password prompt from raw mode.
///     This is NOT a refinement — it is the whole thing. An interactive
///     shell sits at its prompt with ECHO *already off*, because zle (and
///     readline) echo characters themselves and want the tty raw. Measured
///     on this codebase's actual target:
///
///     ```text
///     zsh at its interactive prompt : ECHO=false  ICANON=false
///     child reading a password      : ECHO=false  ICANON=true
///     back at the zsh prompt        : ECHO=false  ICANON=false
///     ```
///
///     So keying on ECHO alone reports "secret" permanently and the
///     frontend hides its input dock forever. A password read is canonical
///     (the program wants the kernel to hand it a whole line); a shell
///     prompt and a full-screen TUI are not.
///
/// Known gap: a reader that goes fully raw and does its own character
/// handling (some TUI password fields) is indistinguishable from a TUI at
/// this layer and is not detected. Covering that needs a different signal
/// — alternate-screen state or the foreground process group — precisely
/// because the shell's own prompt already looks raw.
#[cfg(unix)]
fn is_secret_input(lflag: libc::tcflag_t) -> bool {
    (lflag & libc::ECHO) == 0 && (lflag & libc::ICANON) != 0
}

/// Read the child's termios from the PTY master and decide whether it is
/// currently reading a secret.
///
/// WHY THIS EXISTS: ArcTerm's input editor is a DOM widget that renders
/// keystrokes locally and only pushes bytes to the PTY on Enter. A real
/// terminal renders nothing by itself — the tty line discipline echoes.
/// That is exactly what `sudo` relies on. Without consulting termios,
/// ArcTerm has no way to know a secret is being typed, and paints the
/// password on screen (and files it in history + AI context).
///
/// WHY THE MASTER FD: for a Unix98 PTY the termios state belongs to the
/// shared line discipline, so `tcgetattr` on the master reflects flags the
/// *slave*-side child set. Verified empirically on macOS.
///
/// We go through `libc` rather than `MasterPty::get_termios()` (which
/// returns a `nix` type) so we don't take a direct `nix` dependency that
/// would need version-lockstep with portable-pty's own.
///
/// Returns `None` when the fd or the syscall is unavailable. Callers MUST
/// treat `None` as "unknown" and leave the UI alone, never as a state they
/// did not observe.
#[cfg(unix)]
fn read_secret_state(master: &(dyn portable_pty::MasterPty + Send)) -> Option<bool> {
    use std::os::unix::io::RawFd;
    let fd: RawFd = master.as_raw_fd()?;
    // SAFETY: `fd` is owned by the live PtyEntry that `master` borrows from,
    // so it stays open for the duration of this call. `tcgetattr` only
    // writes into `t` and never retains the pointer.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
        return None;
    }
    Some(is_secret_input(t.c_lflag))
}

/// Non-unix stub. ArcTerm ships macOS-only, but pty.rs itself is portable
/// and we don't want a `cargo check` on another host to fail here. Windows
/// ConPTY has no termios; reporting "unknown" keeps the frontend in its
/// normal (editor-visible) mode, which is the pre-existing behavior.
#[cfg(not(unix))]
fn read_secret_state(_master: &(dyn portable_pty::MasterPty + Send)) -> Option<bool> {
    None
}

/// Lock the entry's master and sample its secret-input state.
fn read_entry_secret(entry: &PtyEntry) -> Option<bool> {
    let master = entry.master.lock();
    read_secret_state(master.as_ref())
}

pub struct PtyManager {
    // Arc + Mutex so reader threads can hold a reference while IPC handlers
    // mutate the map. Inner Arc<PtyEntry> lets a write/resize call grab a
    // single entry without blocking the whole map.
    entries: Arc<Mutex<HashMap<String, Arc<PtyEntry>>>>,
    /// Paths to shell integration files, if installation succeeded. `None`
    /// means "spawn a bare shell without our hooks" — everything still works,
    /// just without the ArcTerm-managed prompt/cwd reporting.
    shell_paths: Option<shell_hooks::Paths>,
}

impl PtyManager {
    pub fn new(shell_paths: Option<shell_hooks::Paths>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            shell_paths,
        }
    }

    /// Spawn a shell PTY and start its reader thread. Returns the new id
    /// plus the per-session OSC nonce so the frontend can validate inbound
    /// shell-integration sequences against it.
    pub fn spawn(
        &self,
        app: AppHandle,
        cols: u16,
        rows: u16,
    ) -> Result<SpawnResult, String> {
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

        // SECURITY: per-session OSC nonce. See PtyEntry.osc_nonce for the
        // threat model. Uuid::new_v4 is cryptographically random via
        // getrandom; the hyphen-less hex form is 32 chars and fits cleanly
        // into an OSC parameter without needing escaping. The shell hooks
        // read this env var into a shell-local variable and immediately
        // `unset ARCTERM_OSC_NONCE` so it never leaks into child processes.
        let osc_nonce = Uuid::new_v4().simple().to_string();
        cmd.env("ARCTERM_OSC_NONCE", &osc_nonce);

        // Shell integration: the mechanism depends on the shell.
        //   - zsh:  set ZDOTDIR; our zdotdir/.zshrc chain-loads the user's
        //           .zshrc then arcterm.zsh.
        //   - bash: add `--rcfile <bash-rcfile>`; it chain-loads .bashrc
        //           and then arcterm.bash. (--rcfile is honored for
        //           interactive non-login shells, which is what PTYs are.)
        //   - fish: add `-C "source .../arcterm.fish"`; fish always loads
        //           its own config.fish, then runs our -C snippet.
        // ARCTERM_INTEGRATION_DIR + ARCTERM_SESSION are set in all cases
        // so scripts can locate sibling hook files and detect "inside
        // ArcTerm" without string-matching TERM_PROGRAM.
        if let Some(paths) = &self.shell_paths {
            cmd.env("ARCTERM_INTEGRATION_DIR", &paths.integration_dir);
            cmd.env("ARCTERM_SESSION", "1");

            let shell_name = std::path::Path::new(&shell)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            match shell_name {
                "zsh" => {
                    cmd.env("ZDOTDIR", &paths.zdotdir);
                    // -l = login shell. Matters when ArcTerm is launched
                    // from Finder (or any non-Terminal launcher) because
                    // macOS gives such processes a minimal launchd PATH
                    // (no /opt/homebrew/bin). .zprofile is the
                    // conventional place users put `eval "$(brew
                    // shellenv)"` and similar PATH setup, and non-login
                    // zsh skips it entirely — so brew is missing when
                    // .zshrc runs. -l runs .zprofile first.
                    //
                    // Terminal.app always passes -l for the same reason.
                    // We match its convention.
                    cmd.arg("-l");
                }
                "bash" => {
                    // --rcfile replaces the default rc lookup (~/.bashrc).
                    // Our rcfile chain-loads the user's .bashrc first so
                    // their env is intact. CommandBuilder::arg() is
                    // in-place (returns ()), so we call per-arg.
                    cmd.arg("--rcfile");
                    cmd.arg(&paths.bash_rcfile);
                    // Same launchd-PATH problem as zsh; bash reads
                    // .bash_profile / .profile in login mode which is
                    // where brew shellenv typically lives.
                    cmd.arg("-l");
                }
                "fish" => {
                    // -l = login shell for fish too (runs config.fish's
                    // login-specific code paths, important for PATH).
                    cmd.arg("-l");
                    // -C "<cmd>" runs after config.fish. Source our hook
                    // file as a post-init step so user config wins the
                    // first pass but our prompt suppression wins the last.
                    cmd.arg("-C");
                    cmd.arg(format!("source {}", paths.fish_hook.display()));
                }
                _ => {
                    // Unknown shell — we still export the env vars above
                    // so any curious user can source the hooks manually,
                    // but we don't try to inject auto-loading.
                }
            }
        }

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
            last_secret: AtomicBool::new(false),
        });
        self.entries.lock().insert(id.clone(), entry.clone());

        // Reader thread. Blocking read is fine — one thread per PTY is cheap
        // and avoids the complexity of async I/O on raw file descriptors,
        // which differs across macOS/Linux/Windows.
        let app_for_reader = app.clone();
        let id_for_reader = id.clone();
        let entries_for_reader = self.entries.clone();
        // Clone the Arc (not the map) so the per-burst echo sample doesn't
        // have to lock the whole entries map and contend with IPC handlers.
        let entry_for_reader = entry.clone();
        thread::Builder::new()
            .name(format!("pty-reader-{}", &id[..8]))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: child closed master
                        Ok(n) => {
                            // Sample BEFORE forwarding the bytes. sudo
                            // clears ECHO and *then* writes "Password: ",
                            // so by the time this burst exists the flags
                            // are already set. Emitting the state first
                            // means the frontend has torn down its input
                            // dock when the prompt paints; emitting after
                            // would leave a frame where the prompt is
                            // visible and the editor still armed.
                            //
                            // `swap` makes compare-and-emit atomic, so two
                            // bursts arriving close together can't both
                            // decide they saw the same transition.
                            if let Some(secret) = read_entry_secret(&entry_for_reader) {
                                if entry_for_reader.last_secret.swap(secret, Ordering::Relaxed)
                                    != secret
                                {
                                    let _ = app_for_reader.emit(
                                        EVENT_SECRET,
                                        SecretPayload {
                                            id: id_for_reader.clone(),
                                            secret,
                                        },
                                    );
                                }
                            }
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
        Ok(SpawnResult { id, osc_nonce })
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

    /// Is this PTY's child currently reading a secret? Sampled on demand.
    ///
    /// The reader thread already pushes `pty://secret-input` on every
    /// transition, which is what drives the UI in the normal case. This
    /// synchronous query exists to close the startup race: the frontend
    /// calls it before accepting the first keystroke into an empty editor,
    /// so a user who types blind-fast (before the transition event has been
    /// delivered) still gets secure-input mode rather than a rendered
    /// password.
    ///
    /// Deliberately read-only — it does NOT update `last_secret`. Letting the
    /// reader thread stay the single writer of that flag keeps the
    /// transition logic in one place; a redundant event after a synchronous
    /// detection is harmless because the frontend's handler is idempotent.
    pub fn secret_input_state(&self, id: &str) -> Result<bool, String> {
        let entry = self.get(id)?;
        read_entry_secret(&entry)
            .ok_or_else(|| format!("secret-input state unavailable for pty {id}"))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use portable_pty::{native_pty_system, PtySize};

    fn test_size() -> PtySize {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// The decision rule, exhaustively. Every one of these four cases
    /// actually occurs in a live session, so none of them is hypothetical.
    #[test]
    fn secret_input_requires_echo_off_and_canonical_on() {
        // Password prompt: sudo/ssh/gpg clear ECHO but read a whole line.
        assert!(is_secret_input(libc::ICANON));

        // REGRESSION: an interactive zsh sits at its prompt with ECHO off
        // AND ICANON off, because zle echoes characters itself. A rule of
        // "ECHO is off => secret" therefore fires permanently and hides the
        // input dock forever — which is exactly the bug this test exists to
        // prevent from coming back. Measured, not assumed:
        //     zsh at prompt : ECHO=false ICANON=false
        //     getpass read  : ECHO=false ICANON=true
        assert!(
            !is_secret_input(0),
            "raw mode (shell prompt / TUI) must not be mistaken for a secret"
        );

        // Ordinary cooked terminal, nothing special happening.
        assert!(!is_secret_input(libc::ECHO | libc::ICANON));

        // ECHO on but non-canonical: still not a secret, nothing is hidden.
        assert!(!is_secret_input(libc::ECHO));
    }

    #[test]
    fn fresh_pty_is_not_reading_a_secret() {
        let pair = native_pty_system()
            .openpty(test_size())
            .expect("openpty failed");
        assert_eq!(
            read_secret_state(pair.master.as_ref()),
            Some(false),
            "a newly-opened PTY is cooked (ECHO+ICANON); reporting `true` \
             here would hide the input dock at startup"
        );
    }

    /// The load-bearing assertion for the whole feature: the flags a
    /// slave-side child sets are visible through the master fd we hold.
    #[test]
    fn secret_input_is_observable_from_the_master() {
        let pair = native_pty_system()
            .openpty(test_size())
            .expect("openpty failed");
        let fd = pair.master.as_raw_fd().expect("master should expose an fd");

        let set_lflags = |on: libc::tcflag_t, off: libc::tcflag_t| {
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(fd, &mut t) }, 0, "tcgetattr");
            t.c_lflag |= on;
            t.c_lflag &= !off;
            assert_eq!(
                unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) },
                0,
                "tcsetattr"
            );
        };

        assert_eq!(read_secret_state(pair.master.as_ref()), Some(false));

        // Drop ECHO the way sudo's tgetpass() does, keeping canonical mode.
        set_lflags(libc::ICANON, libc::ECHO);
        assert_eq!(
            read_secret_state(pair.master.as_ref()),
            Some(true),
            "master must observe a password read"
        );

        // Now drop ICANON too — this is a shell prompt or a TUI, not a
        // secret, even though ECHO is still off.
        set_lflags(0, libc::ICANON);
        assert_eq!(
            read_secret_state(pair.master.as_ref()),
            Some(false),
            "raw mode must not latch the UI into secure-input mode"
        );

        // ...and back to cooked, so the dock restores instead of sticking.
        set_lflags(libc::ECHO | libc::ICANON, 0);
        assert_eq!(read_secret_state(pair.master.as_ref()), Some(false));
    }
}
