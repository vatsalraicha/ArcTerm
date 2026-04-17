/**
 * App bootstrap.
 *
 * Phase 3 wiring:
 *   - Input editor submits -> record to history (Rust SQLite) -> insert a
 *     block-start separator in xterm -> send command to PTY.
 *   - OSC 133;D from the shell -> update that history row with exit code
 *     and duration -> write a block-end separator.
 *   - OSC 7 -> prompt bar cwd update (inherited from Phase 2).
 *   - OSC 1337 ArcTermBranch -> prompt bar git-branch chip.
 *   - ↑ in editor -> open history overlay in browse mode.
 *   - Ctrl+R in editor -> open history overlay in search mode.
 *   - Typing in editor -> debounced autosuggest query against history ->
 *     ghost text in the editor; → or Tab accepts.
 */
import { invoke } from "@tauri-apps/api/core";

import { setupTerminal, type TerminalHandle } from "./terminal";
import { InputEditor } from "./input-editor";
import { HistoryOverlay } from "./history-overlay";

window.addEventListener("DOMContentLoaded", () => {
  const termHost = document.getElementById("terminal");
  const editorHost = document.getElementById("input-editor-host");
  const cwdLabel = document.getElementById("prompt-cwd");
  const branchLabel = document.getElementById("prompt-branch");
  const overlayHost = document.getElementById("app");

  if (!termHost || !editorHost || !cwdLabel || !branchLabel || !overlayHost) {
    throw new Error(
      "ArcTerm: required mount points missing from index.html " +
        "(#terminal, #input-editor-host, #prompt-cwd, #prompt-branch, #app).",
    );
  }

  boot({ termHost, editorHost, cwdLabel, branchLabel, overlayHost }).catch(
    (err) => console.error("ArcTerm boot failed", err),
  );
});

interface Mounts {
  termHost: HTMLElement;
  editorHost: HTMLElement;
  cwdLabel: HTMLElement;
  branchLabel: HTMLElement;
  overlayHost: HTMLElement;
}

async function boot(mounts: Mounts): Promise<void> {
  const terminal: TerminalHandle = await setupTerminal(mounts.termHost);

  // --- Prompt bar state (cwd + branch) ---------------------------------
  // pty.rs starts the shell in $HOME, so the very first OSC 7 is $HOME —
  // cache it for `~` prettification without a separate IPC call.
  let home: string | null = null;
  terminal.onCwdChange((cwd) => {
    if (home === null) home = cwd;
    mounts.cwdLabel.textContent = prettifyCwd(cwd, home);
    mounts.cwdLabel.title = cwd;
  });
  terminal.onBranchChange((branch) => {
    if (branch) {
      mounts.branchLabel.textContent = branch;
      mounts.branchLabel.classList.remove("hidden");
    } else {
      mounts.branchLabel.classList.add("hidden");
      mounts.branchLabel.textContent = "";
    }
  });

  // --- Open command tracking -------------------------------------------
  // A command is "in flight" between the user pressing Enter and the
  // shell emitting OSC 133;D. We keep at most one in flight because the
  // PTY is strictly serial — zsh can't run two commands concurrently
  // in the same shell. Stored data is what we need to finalize the
  // history row when the end marker arrives.
  interface OpenCommand {
    historyId: number | null;
    startedAt: number; // performance.now() for precise duration
  }
  let openCommand: OpenCommand | null = null;

  terminal.onCommandEnd((exitCode) => {
    const open = openCommand;
    openCommand = null;
    const durationMs = open ? Math.round(performance.now() - open.startedAt) : 0;
    terminal.writeBlockEnd(exitCode, open ? durationMs : null);
    if (open && open.historyId !== null) {
      invoke("history_update_exit", {
        id: open.historyId,
        exitCode,
        durationMs,
      }).catch((err) => console.error("history_update_exit failed", err));
    }
  });

  // --- History overlay -------------------------------------------------
  const overlay = new HistoryOverlay({
    host: mounts.overlayHost,
    onSelect: (command) => {
      editor.setValue(command);
      editor.focus();
    },
    onDismiss: () => editor.focus(),
    getCwd: () => terminal.getCwd(),
  });

  // --- Input editor ----------------------------------------------------
  const editor = new InputEditor({
    host: mounts.editorHost,
    onSubmit: (command) => {
      // Empty submit: just echo a blank line so the user sees something
      // happen (matches real-zsh behavior where Enter on an empty prompt
      // re-prints the prompt).
      if (!command.trim()) {
        terminal.send("\r").catch(() => {});
        return;
      }
      // 1. Visual separator + command header in xterm.
      terminal.writeBlockStart(command);
      // 2. Record in history (fire-and-forget; we store the id if it
      //    resolves before the shell finishes — rare but possible for
      //    instant commands like `true`).
      const startedAt = performance.now();
      openCommand = { historyId: null, startedAt };
      const cwd = terminal.getCwd();
      invoke<number>("history_insert", {
        command,
        cwd,
        startedAt: Math.floor(Date.now() / 1000),
        sessionId: terminal.sessionId,
      })
        .then((id) => {
          // If the command finished before this resolved, openCommand is
          // already null — we write the id directly to the backend via
          // a standalone update call. Keeping this branch small avoids
          // the need for a cross-async-result buffer.
          if (openCommand) openCommand.historyId = id;
        })
        .catch((err) => console.error("history_insert failed", err));
      // 3. Send to PTY.
      terminal.send(command + "\r").catch((err) =>
        console.error("send command failed", err),
      );
    },
    onInterrupt: () => {
      terminal.send("\x03").catch(() => {});
    },
    onHistoryUp: () => overlay.open("browse"),
    onSearchHistory: () => overlay.open("search"),
    suggestFor: async (prefix) => {
      try {
        return await invoke<string | null>("history_autosuggest", {
          prefix,
          cwd: terminal.getCwd(),
        });
      } catch (err) {
        console.error("history_autosuggest failed", err);
        return null;
      }
    },
  });
}

/**
 * Turn `/Users/alice/Code/x` into `~/Code/x` for display. Display-only —
 * the value sent to the shell or stored in history is always absolute.
 */
function prettifyCwd(cwd: string, home: string | null): string {
  if (home && cwd === home) return "~";
  if (home && cwd.startsWith(home + "/")) return "~" + cwd.slice(home.length);
  return cwd;
}
