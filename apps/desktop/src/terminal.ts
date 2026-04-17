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

export async function setupTerminal(host: HTMLElement): Promise<void> {
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
  // the PTY — otherwise the shell starts at xterm's default 80x24.
  fit.fit();

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

  // Terminal -> PTY. xterm's onData fires for both keystrokes and pasted
  // content; we forward verbatim and let the shell handle line discipline.
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

  term.focus();
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
