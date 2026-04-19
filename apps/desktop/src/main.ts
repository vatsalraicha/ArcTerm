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
import { listen } from "@tauri-apps/api/event";

import { SessionManager, type Session } from "./session-manager";
import { InputEditor } from "./input-editor";
import { HistoryOverlay } from "./history-overlay";
import { Sidebar } from "./sidebar";
import { AiPanel, type ExplainTarget } from "./ai-panel";
import { aiAsk, aiIsAvailable, extractCommand, type AiContext } from "./ai";
import { CompletionOverlay, type CompletionItem } from "./completion-overlay";
import {
  isInternalCommand,
  registerThemeApplier,
  runInternalCommand,
} from "./arcterm-commands";
import { SettingsPanel } from "./settings-panel";
import type { ThemeName } from "./terminal";

window.addEventListener("DOMContentLoaded", () => {
  const stackHost = requireEl("terminal-stack");
  const editorHost = requireEl("input-editor-host");
  const cwdLabel = requireEl("prompt-cwd");
  const branchLabel = requireEl("prompt-branch");
  const aiStatus = requireEl("ai-status");
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
    aiStatus,
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
  aiStatus: HTMLElement;
  overlayHost: HTMLElement;
  sidebarRoot: HTMLElement;
  sessionListEl: HTMLUListElement;
  newBtn: HTMLButtonElement;
  sidebarSearch: HTMLInputElement;
}

async function boot(mounts: Mounts): Promise<void> {
  const manager = new SessionManager(mounts.stackHost);

  // --- Theme ---------------------------------------------------------
  //
  // Apply the saved theme BEFORE the first session is created so xterm
  // gets constructed with the right palette (no dark→light flash on
  // boot for light-theme users). Theme lives on <html class="theme-*">
  // so every CSS var cascades correctly.
  const savedTheme = await readSavedTheme();
  applyTheme(savedTheme, manager);
  registerThemeApplier((next) => applyTheme(next, manager));

  // --- AI local-model status pill ------------------------------------
  //
  // Wave 2.5: the backend defers the 15–40 s SHA-verify + GGUF mmap
  // off the boot critical path. While it runs in the background, we
  // reflect state in the toolbar so the user understands why local AI
  // isn't instantly available.
  //
  // Subscribe before the initial `ai_status` fetch so we don't miss a
  // "ready" event that fires between fetch and listener registration.
  wireAiStatusPill(mounts.aiStatus);

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
  //
  // `?` prefix: natural-language shortcut. User types "? find all python
  // files modified today" + Enter. We intercept the submit, ask the AI
  // (Command mode, same as ⌘K), replace the editor contents with the
  // returned command, and leave the caret at the end. The user reviews
  // and hits Enter again to execute.
  //
  // Why replace-editor instead of auto-execute: commands generated by an
  // AI can be destructive (`rm -rf`), and the two-step "see, then run"
  // pattern matches how ⌘K's "Edit" button already behaves.
  /**
   * `?` prefix runs the AI asynchronously and replaces the editor
   * contents with the generated command. We intentionally DO NOT write
   * anything to the xterm buffer here — earlier versions did for a
   * "thinking…" status line, but that interleaved with xterm's async
   * write queue and left stale cursor state by the time the user
   * submitted the resulting command, which caused the shell to never
   * see the submission. Visible symptom: command echoed in the buffer
   * but never ran.
   *
   * Feedback during the wait: the editor keeps the user's original
   * `? query` visible (no clear) and we toggle an
   * `arcterm-editor-loading` class so CSS can dim / animate it. On
   * success we replace; on error/empty-result we leave the query
   * intact so the user can retry without retyping.
   */
  const runAiShortcut = async (query: string, active: Session) => {
    if (!query) {
      editor.setValue("? ");
      editor.focus();
      return;
    }
    const editorEl = mounts.editorHost.firstElementChild as HTMLElement | null;
    editorEl?.classList.add("arcterm-editor-loading");
    try {
      const resp = await aiAsk({
        prompt: query,
        mode: "command",
        context: sessionContext(active),
      });
      const cmd = extractCommand(resp.text);
      if (!cmd) {
        console.warn(`ai: no command returned for "${query}"`);
        editor.setValue(`? ${query}`);
        return;
      }
      // Swap the editor contents for the generated command; user
      // reviews, then hits Enter again to actually execute.
      editor.setValue(cmd);
      editor.focus();
    } catch (err) {
      console.error(
        `ai error for "${query}":`,
        err instanceof Error ? err.message : err,
      );
      editor.setValue(`? ${query}`);
    } finally {
      editorEl?.classList.remove("arcterm-editor-loading");
    }
  };

  // Submission logic lives in `submitCommand` so both user-typed commands
  // and AI-panel "Run" buttons take the same path (block header + history
  // insert + PTY send). Empty-command behavior is preserved here: bare Enter
  // pushes a \r to the shell so it still feels responsive.
  const submitCommand = (active: Session, command: string) => {
    // Header pill carries cwd + branch, NOT the command — zle's echo
    // of the command becomes the visible "command line" so we don't
    // render it ourselves. Prettify cwd for display only; the literal
    // path in session state is unchanged.
    const prettyCwd = active.state.cwd
      ? prettifyCwd(active.state.cwd, home)
      : null;
    active.terminal.writeBlockStart(prettyCwd, active.state.branch);
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
  };

  // --- Tab-completion dropdown ----------------------------------------
  // Mounted into the input dock so it visually anchors above the editor.
  // The editor delegates to it via four callbacks (completeFor, show,
  // handleKey, close) — editor doesn't import it directly.
  const inputDock = mounts.editorHost.closest("#input-dock") as HTMLElement;
  const completionOverlay = new CompletionOverlay({
    host: inputDock,
    onPick: () => {
      /* replaced per-call via showCompletions callback below */
    },
  });

  const editor = new InputEditor({
    host: mounts.editorHost,
    onSubmit: (command) => {
      const active = manager.active;
      if (!active) return;
      if (!command.trim()) {
        active.terminal.send("\r").catch(() => {});
        return;
      }
      // ArcTerm internal commands (`/arcterm-*`) are handled in-app: they
      // don't hit the PTY. We still record them in history via the normal
      // path so they're discoverable with ↑, but skip the shell roundtrip.
      if (isInternalCommand(command)) {
        runInternalCommand(command, active).catch((err) =>
          console.error("internal command failed", err),
        );
        return;
      }
      // `?` prefix — natural-language → shell command, inline. Same
      // semantics as the AI panel's Command mode, but without popping a
      // modal. The AI's answer replaces the editor contents; the user
      // reviews, then hits Enter again to actually execute. If AI isn't
      // available, we fall through to a literal submit so the user isn't
      // silently trapped.
      if (aiAvailable && command.startsWith("?")) {
        void runAiShortcut(command.slice(1).trim(), active);
        return;
      }
      submitCommand(active, command);
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
    completeFor: async (text, cursorPos) => {
      try {
        const cwd = manager.active?.state.cwd ?? null;
        const result = await invoke<{
          token_start: number;
          token_end: number;
          completions: CompletionItem[];
        }>("fs_complete", { text, cursorPos, cwd });
        return {
          tokenStart: result.token_start,
          tokenEnd: result.token_end,
          completions: result.completions,
        };
      } catch (err) {
        console.error("fs_complete failed", err);
        return { tokenStart: 0, tokenEnd: 0, completions: [] };
      }
    },
    showCompletions: (items, onPick) => {
      // Pass a per-call onPick so the editor can re-derive splice offsets
      // from the live caret at commit time (the user may have typed more
      // characters between opening and picking).
      completionOverlay.open(items, (item) => onPick(item.replacement));
    },
    completionHandlesKey: (ev) => completionOverlay.handleKey(ev),
    closeCompletions: () => completionOverlay.close(),
  });

  // Close the block on command end for the session that emitted it. We
  // subscribe inside the manager at session-create time; here we only
  // need to handle the history row update, the visual block-end write,
  // and capturing the command's output (for AI explain flows).
  manager.onSessionAdded((s) => {
    s.terminal.onCommandEnd((exitCode) => {
      const open = s.state.openCommand;
      const durationMs = open
        ? Math.round(performance.now() - open.startedAt)
        : 0;
      const captured = s.terminal.writeBlockEnd(
        exitCode,
        open ? durationMs : null,
      );
      s.state.lastOutput = captured || null;
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

  // --- Settings panel --------------------------------------------------
  // Triggered by ⌘, (standard macOS convention) AND by the native
  // `ArcTerm → Settings…` menu item (which emits `menu://settings`
  // from the Rust side). Lets the user flip theme + AI mode + local
  // model + Claude path from one form instead of memorizing three
  // slash-commands. Save is live-apply: theme flips immediately,
  // router swaps mode immediately, rest persists.
  const settingsPanel = new SettingsPanel({
    host: mounts.overlayHost,
    applyTheme: (next) => applyTheme(next, manager),
    focusEditor: () => editor.focus(),
  });
  // Menu → Settings. No unlisten wired because the listener lives for
  // the whole app lifetime; tidy teardown happens at process exit.
  void listen("menu://settings", () => {
    if (!settingsPanel.isOpen()) void settingsPanel.open();
  });

  // --- Sidebar ---------------------------------------------------------
  const sidebar = new Sidebar({
    root: mounts.sidebarRoot,
    listEl: mounts.sessionListEl,
    newBtn: mounts.newBtn,
    searchInput: mounts.sidebarSearch,
    manager,
  });

  // --- AI panel --------------------------------------------------------
  //
  // Availability check gates the keybindings: if `claude` isn't on PATH
  // we leave the shortcuts unbound rather than opening a broken panel.
  // The check is cached — one invocation at startup, plus a re-check
  // on `ai://local-ready` so users whose only backend is the local
  // model (Claude not installed) get the shortcuts bound as soon as
  // the background load finishes.
  let aiAvailable = await aiIsAvailable();
  void listen("ai://local-ready", async () => {
    if (!aiAvailable) {
      aiAvailable = await aiIsAvailable(true);
    }
  });
  const aiPanel = new AiPanel({
    host: mounts.overlayHost,
    runCommand: (cmd) => {
      const active = manager.active;
      if (!active) return;
      // Use the full submit path so the block header, history recording,
      // and PTY send all happen the same way as a user-typed command.
      submitCommand(active, cmd);
    },
    populateEditor: (text) => {
      editor.setValue(text);
      editor.focus();
    },
    getContext: () => sessionContext(manager.active),
    getExplainTarget: () => {
      const s = manager.active;
      if (!s) return null;
      // Prefer the last error, but fall back to whatever's in the editor.
      if (
        s.state.lastExitCode !== null &&
        s.state.lastExitCode !== 0 &&
        s.state.lastCommand
      ) {
        return {
          kind: "error",
          command: s.state.lastCommand,
          output: s.state.lastOutput ?? "",
          exitCode: s.state.lastExitCode,
        };
      }
      const editorText = editor.getValue().trim();
      if (editorText) {
        return { kind: "command", command: editorText };
      }
      // Last resort: the last successful command, for "explain what I just did".
      if (s.state.lastCommand) {
        return { kind: "command", command: s.state.lastCommand };
      }
      return null;
    },
    focusEditor: () => editor.focus(),
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

    // ⌘⇧] / ⌘⇧[  — next / previous tab (matches VSCode convention).
    //
    // Match on ev.code (physical key), not ev.key. When Shift is held,
    // ev.key transforms `[` → `{` and `]` → `}` on a US layout, so the
    // earlier `ev.key === "["` check never fired. ev.code is layout-
    // independent and unaffected by modifiers.
    if (ev.shiftKey && (ev.code === "BracketRight" || ev.code === "BracketLeft")) {
      ev.preventDefault();
      const sessions = manager.list();
      if (sessions.length < 2) return;
      const curId = manager.activeSessionId;
      const curIdx = sessions.findIndex((s) => s.id === curId);
      if (curIdx === -1) return;
      const delta = ev.code === "BracketRight" ? 1 : -1;
      const next = sessions[(curIdx + delta + sessions.length) % sessions.length];
      void manager.switchTo(next.id);
      return;
    }

    // ⌘, — open settings panel. macOS convention; available regardless
    // of AI availability.
    if (ev.key === "," && !ev.shiftKey) {
      ev.preventDefault();
      if (!settingsPanel.isOpen()) void settingsPanel.open();
      return;
    }

    // ⌘K — open AI panel in command-generation mode.
    // ⌘⇧E — open AI panel in explain mode (uses last error, falls back to
    //       editor contents or last command if no error is on record).
    if (!aiAvailable) return;
    if (ev.key === "k" && !ev.shiftKey) {
      ev.preventDefault();
      if (!aiPanel.isOpen()) void aiPanel.openForCommand();
      return;
    }
    if (ev.shiftKey && ev.key.toLowerCase() === "e") {
      ev.preventDefault();
      if (!aiPanel.isOpen()) void aiPanel.openForExplain();
      return;
    }
  });

  // --- First session ---------------------------------------------------
  await manager.create();
  // The manager fires `active-changed` during create → renderPromptBar
  // already ran with the new session. Just focus the editor.
  editor.focus();
}

/**
 * Toolbar pill that reflects the Wave 2.5 background local-model load:
 * loading → ready → (fade out) or loading → failed.
 *
 * Listens for three backend events emitted from `lib.rs::spawn_local_load`:
 *   - `ai://local-loading`      → {id, display_name, quantization}
 *   - `ai://local-ready`        → {id, display_name, quantization}
 *   - `ai://local-load-failed`  → {id, error}
 *
 * Also does a one-shot `ai_status` IPC to derive initial state — covers
 * the frontend-reload (⌘R) case where the backend's load is already in
 * flight and we'd otherwise miss the "loading" event that fired at boot.
 *
 * Claude-only mode: the backend's setup hook short-circuits before
 * emitting any events and `local_loading` in `ai_status` is null, so
 * the pill stays hidden. No UX artifact for users who don't use local.
 */
function wireAiStatusPill(el: HTMLElement): void {
  type LoadPayload = { id: string; display_name: string; quantization?: string };
  type ProgressPayload = {
    id: string;
    phase: "verify" | "compiling";
    percent: number;
  };
  type FailPayload = { id: string; error: string };

  const show = (cls: "loading" | "ready" | "failed", text: string, title?: string) => {
    el.classList.remove("hidden", "loading", "ready", "failed");
    el.classList.add(cls);
    el.textContent = text;
    if (title) el.title = title;
    else el.removeAttribute("title");
  };
  const hide = () => {
    el.classList.add("hidden");
    el.classList.remove("loading", "ready", "failed");
    el.textContent = "";
    el.removeAttribute("title");
  };

  // Strip the parenthetical variant details from display names for the
  // pill — "Gemma 4 E4B (Q8_0, high quality)" → "Gemma 4 E4B". The full
  // label (including quant + params) goes into the hover tooltip so no
  // information is lost.
  const shortName = (name: string): string =>
    name.replace(/\s*\(.*\)\s*$/, "").trim();

  const fullLabel = (p: { display_name: string; quantization?: string }): string =>
    p.quantization && !p.display_name.includes(p.quantization)
      ? `${p.display_name} (${p.quantization})`
      : p.display_name;

  let readyFadeTimer: number | undefined;
  const clearFadeTimer = () => {
    if (readyFadeTimer !== undefined) {
      window.clearTimeout(readyFadeTimer);
      readyFadeTimer = undefined;
    }
  };

  // Cache the short display name from the initial "loading" event so
  // progress ticks don't need to re-derive it (progress payloads only
  // carry id + phase + percent). Elapsed time is derived from
  // `loadStartedAt`; we redraw every second via `tickTimer` so the
  // counter stays fresh even between integer-percent boundaries.
  let currentShortName = "";
  let currentFullLabel = "";
  let currentPhase: "verify" | "compiling" = "verify";
  let currentPercent = 0;
  let loadStartedAt: number | null = null;
  let tickTimer: number | undefined;

  const formatElapsed = (ms: number): string => {
    const secs = Math.floor(ms / 1000);
    if (secs < 60) return `${secs}s`;
    return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  };

  const renderPill = () => {
    const elapsed = loadStartedAt !== null
      ? ` · ${formatElapsed(performance.now() - loadStartedAt)}`
      : "";
    const label = currentShortName || "model";
    if (currentPhase === "verify") {
      show(
        "loading",
        `Verifying · ${label} · ${currentPercent}%${elapsed}`,
        currentFullLabel || undefined,
      );
    } else {
      show(
        "loading",
        `Loading model · ${label}${elapsed}`,
        currentFullLabel || undefined,
      );
    }
  };

  const stopTick = () => {
    if (tickTimer !== undefined) {
      window.clearInterval(tickTimer);
      tickTimer = undefined;
    }
    loadStartedAt = null;
  };

  listen<LoadPayload>("ai://local-loading", (ev) => {
    clearFadeTimer();
    currentShortName = shortName(ev.payload.display_name);
    currentFullLabel = fullLabel(ev.payload);
    currentPhase = "verify";
    currentPercent = 0;
    loadStartedAt = performance.now();
    renderPill();
    stopTick();
    tickTimer = window.setInterval(renderPill, 1000);
  });

  listen<ProgressPayload>("ai://local-loading-progress", (ev) => {
    currentPhase = ev.payload.phase;
    currentPercent = ev.payload.percent;
    // If we never saw the initial "loading" event (e.g. frontend reload
    // mid-verify), still start ticking from now so the user gets a
    // reasonable elapsed counter. It's a relative undercount but
    // better than showing nothing.
    if (loadStartedAt === null) {
      loadStartedAt = performance.now();
      if (tickTimer === undefined) {
        tickTimer = window.setInterval(renderPill, 1000);
      }
    }
    renderPill();
  });

  listen<LoadPayload>("ai://local-ready", (ev) => {
    clearFadeTimer();
    // Capture the total elapsed time for a one-off "ready in Xs" hint —
    // tells the user roughly what to expect on future boots.
    const elapsedSuffix = loadStartedAt !== null
      ? ` · ${formatElapsed(performance.now() - loadStartedAt)}`
      : "";
    stopTick();
    show(
      "ready",
      `${shortName(ev.payload.display_name)} · ready${elapsedSuffix}`,
      fullLabel(ev.payload),
    );
    // Auto-hide the "ready" confirmation after 4 s. Loading and failed
    // states are persistent — the former because progress matters, the
    // latter so the user can investigate.
    readyFadeTimer = window.setTimeout(() => hide(), 4000);
  });

  listen<FailPayload>("ai://local-load-failed", (ev) => {
    clearFadeTimer();
    stopTick();
    show("failed", "Load failed", ev.payload.error);
  });

  // Derive initial state in case a load is already in flight (frontend
  // reload mid-boot). `ai_status` returns a `local_loading` object when
  // one is running; otherwise nothing to do.
  void (async () => {
    try {
      const status = await invoke<{
        local_loading?: { id: string; display_name: string; quantization?: string } | null;
        local_available: boolean;
      }>("ai_status");
      if (status.local_loading) {
        // Populate the pill state as if the `ai://local-loading` event
        // had just fired. Without this, a race where the backend emits
        // `local-loading` before the frontend registers the listener
        // leaves `currentShortName` empty, and subsequent progress
        // events render the "model" generic fallback instead of
        // "Gemma 4 E4B". Also seeds `loadStartedAt` so the elapsed
        // counter starts ticking — undercounts by whatever delta
        // between backend emit and this fetch, but beats 0.
        currentShortName = shortName(status.local_loading.display_name);
        currentFullLabel = fullLabel(status.local_loading);
        currentPhase = "verify";
        currentPercent = 0;
        loadStartedAt = performance.now();
        renderPill();
        if (tickTimer === undefined) {
          tickTimer = window.setInterval(renderPill, 1000);
        }
      }
    } catch (err) {
      // ai_status is optional — renderer fails gracefully if the router
      // state isn't registered (pure Claude-less + no local installed
      // boot path). Just leave the pill hidden.
      console.debug("ai_status unavailable on initial fetch:", err);
    }
  })();
}

/**
 * Read the saved theme from settings, defaulting to "dark" if the
 * settings IPC fails or returns an unknown value. Bounded to the
 * ThemeName union to prevent mysterious "theme-xyz" classes on <html>.
 */
async function readSavedTheme(): Promise<ThemeName> {
  try {
    const s = await invoke<{ theme?: string }>("settings_get");
    return s.theme === "light" ? "light" : "dark";
  } catch (err) {
    console.warn("settings_get failed on boot; using dark theme", err);
    return "dark";
  }
}

/**
 * Apply a theme everywhere it needs to land:
 *   1. <html class="theme-*"> so all CSS vars swap
 *   2. Live xterm instances in every session (they paint a canvas and
 *      can't read CSS vars — we push a preset imperatively)
 *   3. Future-session default inside SessionManager so newly-created
 *      sessions start with the right palette, not a one-frame flicker
 */
function applyTheme(theme: ThemeName, manager: SessionManager): void {
  const root = document.documentElement;
  root.classList.remove("theme-dark", "theme-light");
  root.classList.add(`theme-${theme}`);
  manager.setInitialTheme(theme);
  manager.applyTheme(theme);
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

/**
 * Build an AiContext from whatever the active session knows. We skip
 * filling in `failing_*` fields here — the AI panel does that when the
 * call is explicitly an "explain the error" request (getExplainTarget).
 */
function sessionContext(active: Session | null): AiContext {
  if (!active) return {};
  return {
    cwd: active.state.cwd,
    git_branch: active.state.branch || null,
    // Rust-side `enrich()` populates shell + recent_commands from the
    // history DB before the prompt is built, so we don't need to mirror
    // that data across IPC here.
  };
}
