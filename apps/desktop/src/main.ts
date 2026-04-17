/**
 * App bootstrap.
 *
 * Phase 2 wiring:
 *   1. Mount xterm.js into #terminal for shell output + TUI programs.
 *   2. Mount the custom InputEditor into #input-editor-host.
 *   3. Submit from the editor -> PTY via `terminal.send(command + "\r")`.
 *   4. Shell broadcasts cwd via OSC 7 -> update #prompt-cwd.
 *   5. Ctrl+C in the editor forwards SIGINT (0x03) to the PTY.
 *
 * Everything richer (blocks, history overlay, autosuggestions, AI) layers
 * on top of these primitives in later phases.
 */
import { setupTerminal, type TerminalHandle } from "./terminal";
import { InputEditor } from "./input-editor";

window.addEventListener("DOMContentLoaded", () => {
  const termHost = document.getElementById("terminal");
  const editorHost = document.getElementById("input-editor-host");
  const cwdLabel = document.getElementById("prompt-cwd");

  if (!termHost || !editorHost || !cwdLabel) {
    // Fail loud during development; silent failure here would be invisible
    // because the terminal never mounts and the user just sees an empty
    // window with no indication why.
    throw new Error(
      "ArcTerm: required mount points missing from index.html " +
        "(#terminal, #input-editor-host, #prompt-cwd).",
    );
  }

  boot(termHost, editorHost, cwdLabel).catch((err) => {
    console.error("ArcTerm boot failed", err);
  });
});

async function boot(
  termHost: HTMLElement,
  editorHost: HTMLElement,
  cwdLabel: HTMLElement,
): Promise<void> {
  const terminal: TerminalHandle = await setupTerminal(termHost);

  // Cwd display. pty.rs starts the shell in $HOME, so the very first OSC 7
  // payload IS HOME — we cache it and use the same reference for every later
  // `~/...` prettification without needing a separate IPC call.
  let home: string | null = null;
  terminal.onCwdChange((cwd) => {
    if (home === null) home = cwd;
    cwdLabel.textContent = prettifyCwd(cwd, home);
    cwdLabel.title = cwd; // full path available on hover
  });

  new InputEditor({
    host: editorHost,
    onSubmit: (command) => {
      // The trailing \r (carriage return) is what a real terminal sends on
      // Enter — zsh's line editor treats it as "line complete, execute now".
      // We pass the command verbatim including any embedded newlines from
      // Shift+Enter; zsh handles multi-line input natively.
      terminal.send(command + "\r").catch((err) =>
        console.error("send command failed", err),
      );
    },
    onInterrupt: () => {
      // 0x03 = Ctrl+C. The PTY forwards this to the foreground process
      // group via normal line-discipline signal handling.
      terminal.send("\x03").catch(() => {});
    },
    // Phase 3 callbacks (history ↑/↓, Tab complete) are intentionally
    // omitted — they arrive with the history engine.
  });
}

/**
 * Turn `/Users/alice/Code/x` into `~/Code/x` for display. We don't modify
 * the value sent to the shell or stored elsewhere — this is display-only.
 */
function prettifyCwd(cwd: string, home: string | null): string {
  if (home && cwd === home) return "~";
  if (home && cwd.startsWith(home + "/")) return "~" + cwd.slice(home.length);
  return cwd;
}
