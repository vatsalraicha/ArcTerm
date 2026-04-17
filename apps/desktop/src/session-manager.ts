/**
 * Session manager — multi-terminal state.
 *
 * Each session owns one PTY (identified by the uuid returned from
 * `pty_spawn`) and one xterm.js instance. Only the active session's
 * terminal frame is visible; the others keep streaming output into their
 * xterm buffers but stay `display:none`, so switching back is instant
 * with no replay needed.
 *
 * Why a single-threaded manager (no worker / no message bus)?
 *   - The heavy I/O path (PTY bytes) is already off the JS thread — it
 *     lives in the Rust reader thread + xterm's renderer.
 *   - At human-realistic session counts (≤ a few dozen) the per-session
 *     bookkeeping here is negligible compared to rendering cost.
 *
 * Events emitted:
 *   - active-changed      : when the active session id changes
 *   - session-added       : a new session was created
 *   - session-removed     : a session was closed and its resources freed
 *   - session-updated     : a session's observable state (name, cwd,
 *                            branch, last command, running flag) changed
 *
 * Listeners use the add/remove pattern — no external event-target machinery.
 */

import { invoke } from "@tauri-apps/api/core";

import { setupTerminal, type TerminalHandle, type ThemeName } from "./terminal";
import { writeWelcome } from "./welcome";

export interface SessionState {
    /** User-facing name. Starts as "Session N"; user can rename via sidebar. */
    name: string;
    /** Last cwd reported by the shell, or null until OSC 7 arrives. */
    cwd: string | null;
    /** Last branch reported by the shell (empty string = not in a repo). */
    branch: string;
    /** Most recently submitted command text (for sidebar preview). */
    lastCommand: string | null;
    /** Exit code of the last command, or null if never run / currently running. */
    lastExitCode: number | null;
    /** True while a command is executing in this session. */
    running: boolean;
    /** performance.now() at command start — used to compute duration. */
    runStartedAt: number | null;
    /** In-flight history row: we fill in exit_code + duration when OSC 133;D lands. */
    openCommand: {
        historyId: number | null;
        startedAt: number;
    } | null;
    /** Captured output of the most recent command (both stdout and stderr).
     *  Populated by main.ts from writeBlockEnd's return value. Used by the
     *  AI explain flow to show Claude what actually failed. Capped at ~8 KB.
     *  Null until a command has finished in this session. */
    lastOutput: string | null;
}

export interface Session {
    /** PTY id from Rust. Also used as the xterm frame's key. */
    id: string;
    /** 1-indexed order for Cmd+1..9 shortcuts. Assigned on creation. */
    ordinal: number;
    /** The xterm handle for this session. */
    terminal: TerminalHandle;
    /** The outer frame element containing the xterm host. */
    frame: HTMLElement;
    /** Mutable session state; edits go through SessionManager methods so
     *  subscribers get notified consistently. */
    state: SessionState;
}

type Listener = (session: Session) => void;
type Unsubscribe = () => void;

export class SessionManager {
    private readonly sessions = new Map<string, Session>();
    /** Preserves creation order for Cmd+N lookups and sidebar rendering. */
    private readonly order: string[] = [];
    private activeId: string | null = null;
    private nextOrdinal = 1;

    /** Mount point where each session's terminal-frame is appended. */
    private readonly stackHost: HTMLElement;

    // Listener sets keyed by event name. Small, direct, no framework.
    private readonly addedListeners = new Set<Listener>();
    private readonly removedListeners = new Set<Listener>();
    private readonly updatedListeners = new Set<Listener>();
    private readonly activeChangedListeners = new Set<(id: string | null) => void>();

    /** Current theme; applied to every session created from now on, and
     *  pushed into existing sessions on change. */
    private currentTheme: ThemeName = "dark";

    constructor(stackHost: HTMLElement) {
        this.stackHost = stackHost;
    }

    /** Set the initial theme before the first session is created so xterm
     *  gets constructed with the right palette (no flash). */
    setInitialTheme(theme: ThemeName): void {
        this.currentTheme = theme;
    }

    /** Push a theme change to every live session's xterm. */
    applyTheme(theme: ThemeName): void {
        this.currentTheme = theme;
        for (const s of this.sessions.values()) {
            s.terminal.setTheme(theme);
        }
    }

    /** Currently active session, or null if none exist (transient state
     *  between creating the manager and creating the first session). */
    get active(): Session | null {
        return this.activeId ? this.sessions.get(this.activeId) ?? null : null;
    }

    get activeSessionId(): string | null {
        return this.activeId;
    }

    /** Read-only view of sessions in creation order. */
    list(): Session[] {
        const out: Session[] = [];
        for (const id of this.order) {
            const s = this.sessions.get(id);
            if (s) out.push(s);
        }
        return out;
    }

    /** Look up by PTY id. */
    get(id: string): Session | null {
        return this.sessions.get(id) ?? null;
    }

    /** Session at 1-indexed ordinal position (for Cmd+1..9). */
    byOrdinal(n: number): Session | null {
        for (const id of this.order) {
            const s = this.sessions.get(id);
            if (s && s.ordinal === n) return s;
        }
        return null;
    }

    // -- Lifecycle ------------------------------------------------------

    /** In-flight create serializer. Prevents two rapid Cmd+T presses (or
     *  a click on the + button while another create is still resolving)
     *  from racing and spawning duplicate PTYs. */
    private createInFlight: Promise<Session> | null = null;

    async create(): Promise<Session> {
        if (this.createInFlight) return this.createInFlight;
        this.createInFlight = this.doCreate();
        try {
            return await this.createInFlight;
        } finally {
            this.createInFlight = null;
        }
    }

    private async doCreate(): Promise<Session> {
        // Build the DOM frame first so we can hand setupTerminal() a real
        // host element. The xterm addon requires the host to be in the
        // document tree (even if hidden) so it can measure font metrics.
        const frame = document.createElement("div");
        frame.className = "terminal-frame hidden"; // visibility toggled on activate
        const host = document.createElement("div");
        host.className = "terminal-host";
        frame.append(host);
        this.stackHost.append(frame);

        // To measure font metrics correctly, briefly unhide the frame —
        // xterm.open() reads computed style. A transient hidden flicker is
        // avoided by using visibility:hidden + a specific sizing frame
        // rather than display:none. Keep it simple: the active frame is
        // rendered; for fresh sessions, we activate them right away below.
        frame.classList.remove("hidden");

        let terminal: TerminalHandle;
        try {
            terminal = await setupTerminal(host, this.currentTheme);
        } catch (err) {
            frame.remove();
            throw err;
        }

        // Welcome banner: printed into the xterm buffer BEFORE the shell
        // gets a chance to draw anything. It scrolls naturally out of view
        // the moment the user runs a command, so we never have to manage
        // visibility ourselves.
        writeWelcome(terminal);

        const ordinal = this.nextOrdinal++;
        const session: Session = {
            id: terminal.sessionId,
            ordinal,
            terminal,
            frame,
            state: {
                name: `Session ${ordinal}`,
                cwd: null,
                branch: "",
                lastCommand: null,
                lastExitCode: null,
                running: false,
                runStartedAt: null,
                openCommand: null,
                lastOutput: null,
            },
        };
        this.sessions.set(session.id, session);
        this.order.push(session.id);

        // Wire per-session terminal events into state changes.
        // These don't need explicit cleanup because the TerminalHandle itself
        // goes away when we close the session (listeners die with it).
        terminal.onCwdChange((cwd) => {
            session.state.cwd = cwd;
            this.emitUpdated(session);
        });
        terminal.onBranchChange((branch) => {
            session.state.branch = branch;
            this.emitUpdated(session);
        });
        terminal.onCommandEnd((exitCode) => {
            const running = session.state.running;
            session.state.running = false;
            session.state.lastExitCode = exitCode;
            if (running) this.emitUpdated(session);
        });

        // Fire the "added" event before switching — sidebar wants to render
        // the row before we try to mark it active.
        for (const l of this.addedListeners) l(session);

        // Activate. If this is the first session, activeId was null and
        // switchTo handles it directly.
        await this.switchTo(session.id);

        return session;
    }

    async switchTo(id: string): Promise<void> {
        const next = this.sessions.get(id);
        if (!next) return;
        if (this.activeId === id) return;

        // Hide current.
        if (this.activeId) {
            const cur = this.sessions.get(this.activeId);
            if (cur) cur.frame.classList.add("hidden");
        }

        next.frame.classList.remove("hidden");
        this.activeId = id;

        // xterm needs a fit() on visibility change: its canvas was sized at
        // 0x0 while hidden, so the new frame must re-measure before painting.
        // Defer to next frame so layout settles first.
        requestAnimationFrame(() => {
            // Nudge xterm via a no-op write; the ResizeObserver inside
            // setupTerminal will pick up the display change and call fit().
            // But some renderers don't react to visibility toggles, so
            // call the terminal's focus() which also triggers a render.
            // Don't actually steal focus — that belongs to the input editor.
            // Instead, dispatch a window resize to force fit debouncer.
            window.dispatchEvent(new Event("resize"));
        });

        for (const l of this.activeChangedListeners) l(id);
    }

    /**
     * Close a session. Kills the PTY and disposes the xterm. If `confirm`
     * is needed (running process), the caller should ask before calling
     * this — SessionManager has no UI concerns.
     */
    async close(id: string): Promise<void> {
        const session = this.sessions.get(id);
        if (!session) return;

        // Pick a neighbor to activate after removal (prefer the previous
        // sibling in order, falling back to the next, else null).
        const idx = this.order.indexOf(id);
        const neighborIdx = idx > 0 ? idx - 1 : idx + 1;
        const neighborId = this.order[neighborIdx] ?? null;

        // Remove from order + map first so listeners see a consistent state.
        this.order.splice(idx, 1);
        this.sessions.delete(id);

        // Kill the PTY + reap the shell. The Rust reader thread notices
        // the close and emits `pty://exit`; that's harmless now that we've
        // already pulled the entry out of the map.
        try {
            await invoke("pty_kill", { id: session.id });
        } catch (err) {
            console.warn("pty_kill failed for", session.id, err);
        }

        session.frame.remove();

        for (const l of this.removedListeners) l(session);

        // Switch active if we just closed the active session.
        if (this.activeId === id) {
            this.activeId = null;
            if (neighborId) {
                await this.switchTo(neighborId);
            } else {
                // No sessions left. Emit so UI can render the empty state.
                for (const l of this.activeChangedListeners) l(null);
            }
        }
    }

    /** Rename a session. Emits `session-updated`. */
    rename(id: string, name: string): void {
        const s = this.sessions.get(id);
        if (!s) return;
        const trimmed = name.trim() || s.state.name;
        if (trimmed === s.state.name) return;
        s.state.name = trimmed;
        this.emitUpdated(s);
    }

    /**
     * Record that a command was submitted in this session. Sidebar uses
     * this to update the row's "last command" preview and running dot.
     * main.ts calls this from its editor onSubmit handler.
     */
    markCommandStart(id: string, command: string, historyId: number | null): void {
        const s = this.sessions.get(id);
        if (!s) return;
        s.state.lastCommand = command;
        s.state.lastExitCode = null;
        s.state.running = true;
        s.state.runStartedAt = performance.now();
        s.state.openCommand = {
            historyId,
            startedAt: s.state.runStartedAt,
        };
        this.emitUpdated(s);
    }

    /** Update the in-flight history row id once the insert resolves. */
    attachHistoryId(id: string, historyId: number): void {
        const s = this.sessions.get(id);
        if (!s || !s.state.openCommand) return;
        s.state.openCommand.historyId = historyId;
    }

    // -- Event subscription -----------------------------------------------

    onSessionAdded(l: Listener): Unsubscribe {
        this.addedListeners.add(l);
        return () => this.addedListeners.delete(l);
    }
    onSessionRemoved(l: Listener): Unsubscribe {
        this.removedListeners.add(l);
        return () => this.removedListeners.delete(l);
    }
    onSessionUpdated(l: Listener): Unsubscribe {
        this.updatedListeners.add(l);
        return () => this.updatedListeners.delete(l);
    }
    onActiveChanged(l: (id: string | null) => void): Unsubscribe {
        this.activeChangedListeners.add(l);
        return () => this.activeChangedListeners.delete(l);
    }

    private emitUpdated(s: Session): void {
        for (const l of this.updatedListeners) l(s);
    }
}
