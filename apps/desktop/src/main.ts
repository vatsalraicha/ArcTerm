/**
 * App bootstrap.
 *
 * Phase 4 wiring (multi-session):
 *   - SessionManager owns all PTYs and xterm instances. Each session has
 *     its own terminal frame; only the active one is visible.
 *   - A single InputEditor drives whichever session is active; onSubmit
 *     dispatches to `manager.active.terminal.send(...)`.
 *   - The prompt bar (cwd + branch) reflects active-session state.
 *   - History overlay is session-scoped by cwd: results for the active
 *     session's cwd rank highest.
 *   - Sidebar + global keybindings (Cmd+T/W/1-9/[/]) cover session nav.
 */
import { invoke } from "@tauri-apps/api/core";

import { SessionManager, type Session } from "./session-manager";
import { InputEditor } from "./input-editor";
import { HistoryOverlay } from "./history-overlay";
import { Sidebar } from "./sidebar";

window.addEventListener("DOMContentLoaded", () => {
  const stackHost = requireEl("terminal-stack");
  const editorHost = requireEl("input-editor-host");
  const cwdLabel = requireEl("prompt-cwd");
  const branchLabel = requireEl("prompt-branch");
  const overlayHost = requireEl("app");
  const sidebarRoot = requireEl("sidebar");
  const sessionListEl = requireEl("session-list") as HTMLUListElement;
  const newBtn = requireEl("new-session-btn") as HTMLButtonElement;
  const sidebarSearch = requireEl("sidebar-search") as HTMLInputElement;

  boot({
    stackHost,
    editorHost,
    cwdLabel,
    branchLabel,
    overlayHost,
    sidebarRoot,
    sessionListEl,
    newBtn,
    sidebarSearch,
  }).catch((err) => console.error("ArcTerm boot failed", err));
});

function requireEl(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`ArcTerm: #${id} missing from index.html`);
  return el;
}

interface Mounts {
  stackHost: HTMLElement;
  editorHost: HTMLElement;
  cwdLabel: HTMLElement;
  branchLabel: HTMLElement;
  overlayHost: HTMLElement;
  sidebarRoot: HTMLElement;
  sessionListEl: HTMLUListElement;
  newBtn: HTMLButtonElement;
  sidebarSearch: HTMLInputElement;
}

async function boot(mounts: Mounts): Promise<void> {
  const manager = new SessionManager(mounts.stackHost);

  // --- Prompt bar ------------------------------------------------------
  // HOME is inferred from the first cwd any session reports (shells start
  // in $HOME). One value across all sessions is fine — HOME doesn't change
  // per session, and the worst case is a path not quite prettified for
  // a couple of frames before the first cwd lands.
  let home: string | null = null;
  const renderPromptBar = (s: Session | null) => {
    if (!s) {
      mounts.cwdLabel.textContent = "~";
      mounts.cwdLabel.removeAttribute("title");
      mounts.branchLabel.classList.add("hidden");
      mounts.branchLabel.textContent = "";
      return;
    }
    const cwd = s.state.cwd;
    if (cwd) {
      if (home === null) home = cwd;
      mounts.cwdLabel.textContent = prettifyCwd(cwd, home);
      mounts.cwdLabel.title = cwd;
    } else {
      mounts.cwdLabel.textContent = "…";
      mounts.cwdLabel.removeAttribute("title");
    }
    if (s.state.branch) {
      mounts.branchLabel.textContent = s.state.branch;
      mounts.branchLabel.classList.remove("hidden");
    } else {
      mounts.branchLabel.classList.add("hidden");
      mounts.branchLabel.textContent = "";
    }
  };
  manager.onActiveChanged(() => renderPromptBar(manager.active));
  manager.onSessionUpdated((s) => {
    if (s.id === manager.activeSessionId) renderPromptBar(s);
  });

  // --- History overlay -------------------------------------------------
  const overlay = new HistoryOverlay({
    host: mounts.overlayHost,
    onSelect: (command) => {
      editor.setValue(command);
      editor.focus();
    },
    onDismiss: () => editor.focus(),
    getCwd: () => manager.active?.state.cwd ?? null,
  });

  // --- Input editor ----------------------------------------------------
  const editor = new InputEditor({
    host: mounts.editorHost,
    onSubmit: (command) => {
      const active = manager.active;
      if (!active) return; // no session yet (shouldn't happen post-boot)

      if (!command.trim()) {
        // Empty Enter: pass a bare \r to the shell for a visual "nudge".
        active.terminal.send("\r").catch(() => {});
        return;
      }

      active.terminal.writeBlockStart(command);
      manager.markCommandStart(active.id, command, null);
      const cwd = active.state.cwd;
      invoke<number>("history_insert", {
        command,
        cwd,
        startedAt: Math.floor(Date.now() / 1000),
        sessionId: active.id,
      })
        .then((id) => manager.attachHistoryId(active.id, id))
        .catch((err) => console.error("history_insert failed", err));

      active.terminal.send(command + "\r").catch((err) =>
        console.error("send command failed", err),
      );
    },
    onInterrupt: () => {
      manager.active?.terminal.send("\x03").catch(() => {});
    },
    onHistoryUp: () => overlay.open("browse"),
    onSearchHistory: () => overlay.open("search"),
    suggestFor: async (prefix) => {
      try {
        return await invoke<string | null>("history_autosuggest", {
          prefix,
          cwd: manager.active?.state.cwd ?? null,
        });
      } catch (err) {
        console.error("history_autosuggest failed", err);
        return null;
      }
    },
  });

  // Close the block on command end for the session that emitted it. We
  // subscribe inside the manager at session-create time; here we only
  // need to handle the history row update and the visual block-end write.
  // SessionManager fires `session-updated` when running flips false, but
  // writing the block-end separator belongs here (it knows about history
  // row updates). So subscribe to the raw TerminalHandle events per session
  // via the onSessionAdded event.
  manager.onSessionAdded((s) => {
    s.terminal.onCommandEnd((exitCode) => {
      const open = s.state.openCommand;
      const durationMs = open
        ? Math.round(performance.now() - open.startedAt)
        : 0;
      s.terminal.writeBlockEnd(exitCode, open ? durationMs : null);
      if (open && open.historyId !== null) {
        invoke("history_update_exit", {
          id: open.historyId,
          exitCode,
          durationMs,
        }).catch((err) => console.error("history_update_exit failed", err));
      }
      s.state.openCommand = null;
    });
  });

  // --- Sidebar ---------------------------------------------------------
  const sidebar = new Sidebar({
    root: mounts.sidebarRoot,
    listEl: mounts.sessionListEl,
    newBtn: mounts.newBtn,
    searchInput: mounts.sidebarSearch,
    manager,
  });

  // --- Global keybindings ----------------------------------------------
  // Attached to window so focus in any child (editor, sidebar search) sees
  // the shortcut. We check `metaKey` (⌘ on macOS); Ctrl is intentionally
  // left to shell-level semantics (Ctrl+C forwards to PTY etc).
  window.addEventListener("keydown", (ev) => {
    if (!ev.metaKey || ev.altKey) return;

    // ⌘T — new session
    if (ev.key === "t" && !ev.shiftKey) {
      ev.preventDefault();
      manager.create().catch((err) =>
        console.error("create session failed", err),
      );
      return;
    }

    // ⌘W — close active session (with running-confirm)
    if (ev.key === "w" && !ev.shiftKey) {
      ev.preventDefault();
      const active = manager.active;
      if (active) void sidebar.closeSession(active.id);
      return;
    }

    // ⌘1..⌘9 — switch to session by ordinal
    if (/^[1-9]$/.test(ev.key) && !ev.shiftKey) {
      ev.preventDefault();
      const s = manager.byOrdinal(Number.parseInt(ev.key, 10));
      if (s) void manager.switchTo(s.id);
      return;
    }

    // ⌘⇧] / ⌘⇧[  — next / previous tab (matches VSCode convention)
    if (ev.shiftKey && (ev.key === "]" || ev.key === "[")) {
      ev.preventDefault();
      const sessions = manager.list();
      if (sessions.length < 2) return;
      const curId = manager.activeSessionId;
      const curIdx = sessions.findIndex((s) => s.id === curId);
      if (curIdx === -1) return;
      const delta = ev.key === "]" ? 1 : -1;
      const next = sessions[(curIdx + delta + sessions.length) % sessions.length];
      void manager.switchTo(next.id);
    }
  });

  // --- First session ---------------------------------------------------
  await manager.create();
  // The manager fires `active-changed` during create → renderPromptBar
  // already ran with the new session. Just focus the editor.
  editor.focus();
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
