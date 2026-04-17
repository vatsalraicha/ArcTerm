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

export type ThemeName = "dark" | "light";

export interface TerminalHandle {
  /** Write raw text to the PTY (e.g. a command line followed by \r). */
  send: (data: string) => Promise<void>;
  /**
   * Write bytes DIRECTLY into the xterm buffer — bypasses the PTY.
   * Used by ArcTerm-internal commands (slash commands, status messages)
   * that want to render output inline without involving the shell.
   * Supports ANSI escape sequences exactly the same way shell output does.
   */
  writeRaw: (data: string) => void;
  /**
   * Swap the xterm theme (bg/fg/cursor/selection/ANSI palette). Called
   * when the user changes the app theme via /arcterm-theme; ArcTerm's
   * chrome updates via a CSS class swap, but xterm manages its own
   * canvas and needs an imperative update.
   */
  setTheme: (theme: ThemeName) => void;
  /** Subscribe to cwd updates emitted via OSC 7 from the shell. */
  onCwdChange: (cb: (cwd: string) => void) => void;
  /** Subscribe to git-branch updates from custom OSC 1337 ArcTermBranch. */
  onBranchChange: (cb: (branch: string) => void) => void;
  /** Fires when the shell finishes a command (OSC 133;D;<exit>). */
  onCommandEnd: (cb: (exitCode: number) => void) => void;
  /** Current known cwd, or null until the shell reports one. */
  getCwd: () => string | null;
  /**
   * Insert a top-of-block header into the terminal output: a dim cwd +
   * optional git branch pill. Does NOT render the command itself —
   * zsh's line editor (zle) echoes the command back to the PTY as it
   * reads our submission, and that echo BECOMES the visible command
   * line. Avoids the duplicate-rendering problem that the previous
   * approach (header + conceal-via-color) ran into.
   *
   * Pass null for cwd if the shell hasn't reported one yet (we'll show
   * a `~` placeholder). branch may be empty for non-repo dirs.
   */
  writeBlockStart: (cwd: string | null, branch: string) => void;
  /** Insert a visual block-end separator with exit code + duration.
   *  Returns the text captured between block-start and block-end — this is
   *  the command's output, which the AI explain flow feeds back to Claude
   *  for error-explain requests. Length-capped internally. */
  writeBlockEnd: (
    exitCode: number | null,
    durationMs: number | null,
  ) => string;
  /** Move keyboard focus into xterm (for TUI programs that need direct keys). */
  focus: () => void;
  /** Unique id for this PTY session — stored with history entries. */
  sessionId: string;
}

/**
 * xterm theme presets. Kept here (not in CSS) because xterm paints a
 * canvas it owns — it can't read CSS variables directly. When the app
 * theme changes, main.ts calls `handle.setTheme(name)` which pushes the
 * matching preset into the live xterm instance.
 */
const XTERM_THEMES: Record<ThemeName, NonNullable<ConstructorParameters<typeof Terminal>[0]>["theme"]> = {
  dark: {
    background: "#1a1a2e",
    foreground: "#e0e0e0",
    cursor: "#4fc3f7",
    cursorAccent: "#1a1a2e",
    selectionBackground: "rgba(79, 195, 247, 0.3)",
  },
  light: {
    background: "#f4f4f4",
    foreground: "#1a1a1a",
    cursor: "#1976d2",
    cursorAccent: "#ffffff",
    selectionBackground: "rgba(25, 118, 210, 0.25)",
  },
};

export async function setupTerminal(
  host: HTMLElement,
  initialTheme: ThemeName = "dark",
): Promise<TerminalHandle> {
  const term = new Terminal({
    fontFamily: '"JetBrains Mono", Menlo, Monaco, monospace',
    fontSize: 14,
    cursorBlink: true,
    cursorStyle: "bar",
    scrollback: 10_000,
    allowProposedApi: true,
    theme: XTERM_THEMES[initialTheme],
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

  // --- OSC 133 handler: block boundary markers -------------------------
  // arcterm.zsh only emits ";D;<exit_code>" — ArcTerm decides block-start
  // itself when the user presses Enter. Parameters are split by ';'.
  const commandEndListeners = new Set<(exitCode: number) => void>();
  term.parser.registerOscHandler(133, (payload: string) => {
    const parts = payload.split(";");
    if (parts[0] === "D") {
      // Exit code defaults to 0 if absent; zsh always sends one though.
      const code = Number.parseInt(parts[1] ?? "0", 10);
      if (!Number.isNaN(code)) {
        for (const cb of commandEndListeners) cb(code);
      }
    }
    return true;
  });

  // --- OSC 1337 handler: ArcTerm custom key/value ----------------------
  // Format emitted by arcterm.zsh: `ArcTermBranch=<name>`. iTerm uses
  // OSC 1337 broadly for its own keys; we namespace with the `ArcTerm`
  // prefix so we can add more keys later without collision.
  const branchListeners = new Set<(branch: string) => void>();
  term.parser.registerOscHandler(1337, (payload: string) => {
    const eq = payload.indexOf("=");
    if (eq === -1) return false; // not key/value; let other handlers try
    const key = payload.slice(0, eq);
    const value = payload.slice(eq + 1);
    if (key === "ArcTermBranch") {
      for (const cb of branchListeners) cb(value);
      return true;
    }
    // Unknown ArcTerm* keys: consume silently so they don't render as garbage.
    if (key.startsWith("ArcTerm")) return true;
    return false;
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

  // Capture state for the currently-open block. We remember the absolute
  // line index inside xterm's buffer at the moment writeBlockStart runs, so
  // that writeBlockEnd can slice the lines in between as "command output".
  // Absolute = `baseY + cursorY`; using absolute indices is buffer-scroll
  // safe.
  let blockStartAbs: number | null = null;

  // Caller decides who holds focus — typically the InputEditor takes it.
  return {
    send: (data: string) => invoke("pty_write", { id: ptyId, data }),
    writeRaw: (data: string) => term.write(data),
    setTheme: (name: ThemeName) => {
      // xterm has an `options.theme` setter that triggers a repaint.
      // Runtime swap; no renderer rebuild needed.
      term.options.theme = XTERM_THEMES[name];
    },
    onCwdChange: (cb) => {
      cwdListeners.add(cb);
      // Replay the current cwd so late subscribers don't miss it.
      if (currentCwd) cb(currentCwd);
    },
    onBranchChange: (cb) => branchListeners.add(cb),
    onCommandEnd: (cb) => commandEndListeners.add(cb),
    getCwd: () => currentCwd,
    writeBlockStart: (cwd: string | null, branch: string) => {
      writeBlockStart(term, cwd, branch);
      // Record where we just left the cursor so writeBlockEnd can
      // capture from this point as "command output". With the new
      // architecture, the first line(s) after this point are zle's
      // echo of the command — that's part of the user-facing block,
      // and including it in the captured output (for AI explain) is
      // useful context.
      const buf = term.buffer.active;
      blockStartAbs = buf.baseY + buf.cursorY;
    },
    writeBlockEnd: (exitCode, durationMs) => {
      // Capture BEFORE writing the footer so our separator lines don't
      // pollute the output we pass to Claude.
      const captured = blockStartAbs !== null
        ? captureBufferRange(term, blockStartAbs, term.buffer.active.baseY + term.buffer.active.cursorY)
        : "";
      writeBlockEnd(term, exitCode, durationMs);
      blockStartAbs = null;
      return captured;
    },
    focus: () => term.focus(),
    sessionId: ptyId,
  };
}

/**
 * Slice xterm's scrollback between two absolute line indices and return
 * the raw text. Used to feed failed-command output into the AI explain
 * flow without needing a separate PTY tee.
 *
 * Length-capped at 8 KB so a command that prints megabytes of output
 * doesn't balloon the prompt we send to Claude. We keep the tail since
 * the tail is almost always where the real error lives.
 */
function captureBufferRange(
  term: Terminal,
  startAbs: number,
  endAbs: number,
): string {
  const MAX_BYTES = 8 * 1024;
  const buf = term.buffer.active;
  const from = Math.max(0, Math.min(startAbs, endAbs));
  const to = Math.max(from, endAbs);
  const lines: string[] = [];
  for (let y = from; y <= to; y++) {
    const line = buf.getLine(y);
    if (!line) continue;
    // translateToString with trimRight drops the trailing spaces xterm
    // pads lines with; we pass true to also strip the cursor's empty cell.
    lines.push(line.translateToString(true));
  }
  let out = lines.join("\n").trimEnd();
  if (out.length > MAX_BYTES) {
    out = "…[truncated]\n" + out.slice(out.length - MAX_BYTES);
  }
  return out;
}

/**
 * Render a block-start separator directly into the xterm buffer and
 * *conceal* the duplicate command echo that zsh's line editor is about to
 * emit.
 *
 * The problem: when we send `command + "\r"` to the PTY, zsh's zle reads
 * the bytes and echoes them character-by-character back through the PTY.
 * Without our hook, the terminal shows:
 *
 *     ❯ python --version     <- our styled header (written here)
 *     python --version       <- zsh's zle echo (duplicate)
 *     <output>
 *
 * The fix: right after writing the header we set the foreground color to
 * match the terminal background. zle's echo still arrives but is visually
 * invisible. The shell's preexec hook (arcterm.zsh) emits OSC 133;C
 * followed by \e[0m just before executing the real command, which resets
 * the color in time for output to render normally.
 *
 * If preexec doesn't fire (user interrupts before execution, non-zsh shell,
 * script files with shell hooks missing) the \e[0m in precmd's ;D marker
 * is our second line of defense. In the pathological case where neither
 * runs, the user sees nothing for the next output; they can type `\e[0m`
 * (echo) to recover — but this is vanishingly unlikely in practice.
 */
/**
 * Top-of-block header: a dim cwd + branch pill. Renders a single line
 * like `📁 ~/Code  ⎇ main`. The command itself is NOT rendered — zle
 * will echo it on the next line(s) as it reads our submission, and
 * that echo becomes the visible command. Eliminates the
 * duplicate-rendering problem at its source.
 */
function writeBlockStart(
  term: Terminal,
  cwd: string | null,
  branch: string,
): void {
  const theme = term.options.theme ?? {};
  const accent = String(theme.cursor ?? "#4fc3f7");
  const accentSgr = hexToFgSgr(accent);

  // Dim attribute for the whole pill — keeps it visible but quieter
  // than command output, consistent with Warp's "subtle metadata pill"
  // pattern. Folder glyph is a graceful unicode degrade for terminals
  // that don't render the emoji.
  const cwdText = cwd ?? "~";
  const branchPart = branch
    ? `  \x1b[2m${accentSgr}⎇ ${branch}\x1b[0m`
    : "";
  term.write(`\x1b[2m📁 ${cwdText}\x1b[0m${branchPart}\r\n`);
}

/**
 * Render a block-end separator: a thin horizontal line with exit-code and
 * duration annotations. Exit 0 gets a dim green check; non-zero gets a red
 * cross plus the code. Duration formatted as ms / s / m based on magnitude.
 */
function writeBlockEnd(
  term: Terminal,
  exitCode: number | null,
  durationMs: number | null,
): void {
  // Status indicators stay theme-independent (their meaning is
  // inherently colored): green check, red cross. Use desaturated
  // versions so they don't punch out of either palette.
  const status =
    exitCode === null
      ? ""
      : exitCode === 0
        ? "\x1b[38;2;102;187;106m✓\x1b[0m"
        : `\x1b[38;2;239;83;80m✗ ${exitCode}\x1b[0m`;
  const dur =
    durationMs === null
      ? ""
      : `\x1b[2m${formatDuration(durationMs)}\x1b[0m`;
  // Separator: thin dim line in theme-foreground color. The dim SGR
  // (\x1b[2m) plus the explicit fg gives a subtle weight that reads
  // correctly on both dark and light backgrounds — a hardcoded gray
  // would be wrong against a light bg.
  const theme = term.options.theme ?? {};
  const lineFg = hexToFgSgr(String(theme.foreground ?? "#e0e0e0"));
  const line = `\x1b[2m${lineFg}` + "─".repeat(Math.max(term.cols - 12, 10)) + "\x1b[0m";
  const suffix = [status, dur].filter(Boolean).join(" ");
  // No leading \r\n: zle's echo + command output naturally end on a
  // newline, so the cursor is already at column 1 of a fresh line by
  // the time we get here. Adding another \r\n created a blank line
  // between the output and the separator that the user flagged as
  // unnecessary "room to breathe."
  term.write(`${line}${suffix ? " " + suffix : ""}\r\n`);
}

/**
 * Convert a CSS hex color (#rrggbb or #rgb) into a 24-bit-truecolor
 * foreground SGR escape. xterm understands `\x1b[38;2;R;G;Bm`
 * universally — works in every renderer (canvas, DOM, WebGL).
 *
 * Falls back to a sane default if the input isn't parseable so a typo
 * in a theme can never produce empty output.
 */
function hexToFgSgr(color: string): string {
  let hex = color.trim();
  if (hex.startsWith("#")) hex = hex.slice(1);
  // Expand #abc → #aabbcc.
  if (hex.length === 3) {
    hex = hex.split("").map((c) => c + c).join("");
  }
  if (hex.length !== 6) {
    return "\x1b[39m"; // default fg
  }
  const r = parseInt(hex.slice(0, 2), 16);
  const g = parseInt(hex.slice(2, 4), 16);
  const b = parseInt(hex.slice(4, 6), 16);
  if (Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b)) {
    return "\x1b[39m";
  }
  return `\x1b[38;2;${r};${g};${b}m`;
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m${s}s`;
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
