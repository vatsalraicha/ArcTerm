/**
 * xterm.js setup + PTY bridge.
 *
 * The Rust backend exposes four Tauri commands (see src-tauri/src/pty.rs):
 *   - pty_spawn(cols, rows)     -> string id   : start a shell, returns PTY id
 *   - pty_write(id, data)       -> ()          : write user keystrokes to PTY
 *   - pty_resize(id, cols,rows) -> ()          : forward terminal resize
 *   - pty_kill(id)              -> ()          : kill child + close PTY
 *
 * The backend pushes shell output via the Tauri event "pty://data" with
 * payload { id, data }. We subscribe once and route by id so multi-session
 * support (Phase 4) only needs another spawn + a session->terminal map.
 *
 * Phase 2 additions:
 *   - Expose a `send(text)` handle so the custom input editor can push
 *     commands into the PTY without the rest of the app having to know
 *     about Tauri IPC.
 *   - Register an OSC 7 handler that fires `onCwdChange` when the shell
 *     announces a working-directory change (via arcterm.zsh's chpwd hook).
 */
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import "@xterm/xterm/css/xterm.css";

// Matches the Rust event name. Keep these two strings in sync.
const PTY_DATA_EVENT = "pty://data";
const PTY_EXIT_EVENT = "pty://exit";

interface PtyDataPayload {
  id: string;
  // base64-encoded bytes from the PTY. Base64 avoids UTF-8 boundary issues
  // when the shell emits multi-byte sequences across read chunks.
  data: string;
}

interface PtyExitPayload {
  id: string;
  code: number | null;
}

export interface TerminalHandle {
  /** Write raw text to the PTY (e.g. a command line followed by \r). */
  send: (data: string) => Promise<void>;
  /** Subscribe to cwd updates emitted via OSC 7 from the shell. */
  onCwdChange: (cb: (cwd: string) => void) => void;
  /** Current known cwd, or null until the shell reports one. */
  getCwd: () => string | null;
  /** Move keyboard focus into xterm (for TUI programs that need direct keys). */
  focus: () => void;
}

export async function setupTerminal(host: HTMLElement): Promise<TerminalHandle> {
  const term = new Terminal({
    fontFamily: '"JetBrains Mono", Menlo, Monaco, monospace',
    fontSize: 14,
    cursorBlink: true,
    cursorStyle: "bar",
    scrollback: 10_000,
    allowProposedApi: true,
    theme: {
      background: "#1a1a2e",
      foreground: "#e0e0e0",
      cursor: "#4fc3f7",
      cursorAccent: "#1a1a2e",
      selectionBackground: "rgba(79, 195, 247, 0.3)",
    },
  });

  const fit = new FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon());

  term.open(host);

  // WebGL renderer must be loaded *after* open(); it attaches a canvas to the
  // DOM. If WebGL is unavailable (e.g. headless CI), fall back to the default
  // DOM renderer rather than crashing the app.
  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => webgl.dispose());
    term.loadAddon(webgl);
  } catch (err) {
    console.warn("ArcTerm: WebGL renderer unavailable, falling back to DOM", err);
  }

  // Initial fit so cols/rows reflect the actual window size before we spawn
  // the PTY — otherwise the shell starts at xterm's default 80x24. We also
  // schedule one more fit on the next frame: fonts and flex layout settle
  // asynchronously, and a stale first fit can leave the canvas clipped at
  // the bottom of the window.
  fit.fit();
  requestAnimationFrame(() => fit.fit());

  // --- OSC 7 handler: shell reports cwd ---------------------------------
  // Format the arcterm.zsh script emits: `file://host/absolute/path`.
  // xterm's registerOscHandler returns true to mark the sequence consumed.
  let currentCwd: string | null = null;
  const cwdListeners = new Set<(cwd: string) => void>();
  term.parser.registerOscHandler(7, (uri: string) => {
    const cwd = parseOsc7(uri);
    if (cwd && cwd !== currentCwd) {
      currentCwd = cwd;
      for (const cb of cwdListeners) cb(cwd);
    }
    return true;
  });

  const ptyId = await invoke<string>("pty_spawn", {
    cols: term.cols,
    rows: term.rows,
  });

  // PTY -> terminal. Decode base64 bytes and feed raw to xterm so escape
  // sequences (colors, cursor moves, DCS for blocks later) are preserved.
  const unlistenData: UnlistenFn = await listen<PtyDataPayload>(
    PTY_DATA_EVENT,
    (event) => {
      if (event.payload.id !== ptyId) return;
      const bytes = base64ToBytes(event.payload.data);
      term.write(bytes);
    },
  );

  const unlistenExit: UnlistenFn = await listen<PtyExitPayload>(
    PTY_EXIT_EVENT,
    (event) => {
      if (event.payload.id !== ptyId) return;
      term.writeln(`\r\n\x1b[33m[process exited: code=${event.payload.code ?? "?"}]\x1b[0m`);
    },
  );

  // Terminal -> PTY. xterm's onData still fires when the user clicks into
  // the output area and types — useful for TUI programs (vim, htop) where
  // the custom editor doesn't apply. The primary input path in Phase 2 is
  // the InputEditor calling `handle.send()` below.
  term.onData((data) => {
    invoke("pty_write", { id: ptyId, data }).catch((err) =>
      console.error("pty_write failed", err),
    );
  });

  // Resize plumbing. We debounce because rapid window resizes generate many
  // events; each fit() recalculates cols/rows and ResizeObserver fires once.
  let resizeTimer: number | undefined;
  const resize = () => {
    window.clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => {
      fit.fit();
      invoke("pty_resize", {
        id: ptyId,
        cols: term.cols,
        rows: term.rows,
      }).catch((err) => console.error("pty_resize failed", err));
    }, 50);
  };

  const ro = new ResizeObserver(resize);
  ro.observe(host);
  window.addEventListener("resize", resize);

  // Tear down on page unload so the Rust side gets a chance to reap the child
  // shell instead of leaving a zombie until the app process exits.
  window.addEventListener("beforeunload", () => {
    unlistenData();
    unlistenExit();
    invoke("pty_kill", { id: ptyId }).catch(() => {});
  });

  // Caller decides who holds focus — typically the InputEditor takes it.
  return {
    send: (data: string) => invoke("pty_write", { id: ptyId, data }),
    onCwdChange: (cb) => {
      cwdListeners.add(cb);
      // Replay the current cwd so late subscribers don't miss it.
      if (currentCwd) cb(currentCwd);
    },
    getCwd: () => currentCwd,
    focus: () => term.focus(),
  };
}

/**
 * Parse an OSC 7 payload. Real-world examples:
 *   "file://mymac.local/Users/vr/Code/Apple/ArcTerm"
 *   "file:///Users/vr"     (empty host — also valid)
 * We ignore the host and return just the decoded path.
 */
function parseOsc7(uri: string): string | null {
  if (!uri.startsWith("file://")) return null;
  // Skip scheme. Then skip past the next "/" to drop the host segment.
  const rest = uri.slice("file://".length);
  const pathStart = rest.indexOf("/");
  if (pathStart === -1) return null;
  const encodedPath = rest.slice(pathStart);
  try {
    return decodeURIComponent(encodedPath);
  } catch {
    // Malformed percent-encoding — return the raw value rather than nothing
    // so the UI can still show *something* sensible.
    return encodedPath;
  }
}

/**
 * Decode a base64 string into a Uint8Array. We avoid `atob` -> string -> bytes
 * conversion in a tight loop since shell output can be megabytes per second
 * during e.g. `cat largefile`.
 */
function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
