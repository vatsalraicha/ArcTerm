/**
 * Sidebar session list.
 *
 * Responsibilities:
 *   - Render one row per session (name, cwd, last command, running dot).
 *   - Route clicks to SessionManager.switchTo().
 *   - Route X-button clicks to SessionManager.close() — with confirmation
 *     when the session has a command in flight.
 *   - Filter rows by the search box (simple case-insensitive substring).
 *   - Rename inline (double-click the name).
 *
 * The sidebar owns no state of its own — it re-reads from SessionManager on
 * every event. Performance is fine at real-world session counts.
 */

import type { SessionManager, Session } from "./session-manager";

export interface SidebarOptions {
    root: HTMLElement;       // #sidebar
    listEl: HTMLUListElement; // #session-list
    newBtn: HTMLButtonElement;
    searchInput: HTMLInputElement;
    manager: SessionManager;
    /** Whether to skip the "process is running" confirmation — useful in
     *  tests or when the user holds a modifier. Defaults false. */
    confirmClose?: (session: Session) => boolean;
}

export class Sidebar {
    private readonly opts: SidebarOptions;
    private filter = "";

    constructor(opts: SidebarOptions) {
        this.opts = opts;

        opts.newBtn.addEventListener("click", () => {
            // The manager creates + activates. No need to await here — the
            // click handler fires-and-forgets; errors surface on the
            // console.error path inside create().
            opts.manager.create().catch((err) =>
                console.error("create session failed", err),
            );
        });

        opts.searchInput.addEventListener("input", () => {
            this.filter = opts.searchInput.value.toLowerCase();
            this.render();
        });

        // Re-render on any session-manager event. Cheap enough to be
        // unconditional; we can micro-optimize with partial updates once the
        // list has hundreds of entries (unlikely for terminal sessions).
        opts.manager.onSessionAdded(() => this.render());
        opts.manager.onSessionRemoved(() => this.render());
        opts.manager.onSessionUpdated(() => this.render());
        opts.manager.onActiveChanged(() => this.render());

        opts.listEl.addEventListener("click", this.onListClick);
        opts.listEl.addEventListener("dblclick", this.onListDblClick);
        opts.listEl.addEventListener("contextmenu", this.onListContextMenu);
    }

    private render(): void {
        const sessions = this.opts.manager.list();
        const activeId = this.opts.manager.activeSessionId;

        const filtered = this.filter
            ? sessions.filter((s) => sessionMatchesFilter(s, this.filter))
            : sessions;

        if (sessions.length === 0) {
            this.opts.listEl.innerHTML = `
                <li class="session-list-empty">No sessions — ⌘T to open one.</li>
            `;
            return;
        }
        if (filtered.length === 0) {
            this.opts.listEl.innerHTML = `
                <li class="session-list-empty">No sessions match "${escapeHtml(this.filter)}".</li>
            `;
            return;
        }

        // Build HTML in a fragment for one DOM swap. Rows are short; this is
        // plenty fast and avoids the diffing cost of a framework.
        const frag = document.createDocumentFragment();
        for (const s of filtered) {
            frag.append(this.buildRow(s, s.id === activeId));
        }
        this.opts.listEl.innerHTML = "";
        this.opts.listEl.append(frag);
    }

    private buildRow(session: Session, active: boolean): HTMLLIElement {
        const li = document.createElement("li");
        li.className = "session-item";
        if (active) li.classList.add("active");

        // Status hint: running > exit-err > exit-ok > neutral.
        // The dot is updated via these classes.
        if (session.state.running) {
            li.classList.add("running");
        } else if (session.state.lastExitCode === 0) {
            li.classList.add("exit-ok");
        } else if (
            session.state.lastExitCode !== null &&
            session.state.lastExitCode !== 0
        ) {
            li.classList.add("exit-err");
        }

        li.dataset.sessionId = session.id;

        const dot = document.createElement("span");
        dot.className = "session-dot";
        dot.setAttribute("aria-hidden", "true");
        dot.title = session.state.running
            ? "Running…"
            : session.state.lastExitCode === 0
                ? "Last command succeeded"
                : session.state.lastExitCode !== null
                    ? `Last exit ${session.state.lastExitCode}`
                    : "Idle";

        const body = document.createElement("div");
        body.className = "session-body";

        const name = document.createElement("div");
        name.className = "session-name";
        name.textContent = session.state.name;
        name.title = `${session.state.name} (Cmd+${session.ordinal})`;

        const meta = document.createElement("div");
        meta.className = "session-meta";
        meta.textContent = session.state.cwd
            ? shortenCwd(session.state.cwd)
            : "…";

        body.append(name, meta);

        if (session.state.lastCommand) {
            const last = document.createElement("div");
            last.className = "session-last";
            last.textContent = `❯ ${session.state.lastCommand}`;
            body.append(last);
        }

        const close = document.createElement("button");
        close.className = "session-close";
        close.type = "button";
        close.textContent = "×";
        close.title = "Close session (⌘W)";
        close.setAttribute("aria-label", "Close session");
        close.dataset.close = "1";

        li.append(dot, body, close);
        return li;
    }

    private readonly onListClick = async (ev: MouseEvent): Promise<void> => {
        const row = (ev.target as HTMLElement).closest(
            ".session-item",
        ) as HTMLElement | null;
        if (!row) return;
        const id = row.dataset.sessionId;
        if (!id) return;

        const target = ev.target as HTMLElement;
        // Close button?
        if (target.dataset.close === "1") {
            ev.stopPropagation();
            await this.closeSession(id);
            return;
        }

        // Regular click → switch. Guard against clicks on the rename-in-
        // progress contenteditable name (handled by dblclick).
        if (target.classList.contains("session-name") &&
            target.getAttribute("contenteditable") === "true") {
            return;
        }
        this.opts.manager.switchTo(id);
    };

    private readonly onListDblClick = (ev: MouseEvent): void => {
        const nameEl = (ev.target as HTMLElement).closest(
            ".session-name",
        ) as HTMLElement | null;
        if (!nameEl) return;
        const row = nameEl.closest(".session-item") as HTMLElement | null;
        if (!row?.dataset.sessionId) return;
        this.beginRename(row.dataset.sessionId, nameEl);
    };

    /**
     * Right-click on a session row → context menu with Rename, Close,
     * Close Others. Discoverability handle for the rename feature
     * (double-click works but isn't obvious until you stumble on it).
     */
    private readonly onListContextMenu = (ev: MouseEvent): void => {
        const row = (ev.target as HTMLElement).closest(
            ".session-item",
        ) as HTMLElement | null;
        if (!row?.dataset.sessionId) return;
        ev.preventDefault();
        const id = row.dataset.sessionId;
        const nameEl = row.querySelector(".session-name") as HTMLElement | null;

        const sessions = this.opts.manager.list();
        const items: ContextMenuItem[] = [
            {
                label: "Rename",
                action: () => {
                    if (nameEl) this.beginRename(id, nameEl);
                },
            },
            {
                label: "Close",
                action: () => void this.closeSession(id),
            },
        ];
        if (sessions.length > 1) {
            items.push({
                label: "Close Others",
                action: () => void this.closeOthers(id),
            });
        }

        showContextMenu(ev.clientX, ev.clientY, items);
    };

    /** Close every session except `keepId`. Confirms each running session
     *  individually so the user doesn't lose in-flight work without notice. */
    private async closeOthers(keepId: string): Promise<void> {
        const others = this.opts.manager
            .list()
            .filter((s) => s.id !== keepId);
        for (const s of others) {
            await this.closeSession(s.id);
        }
    }

    private beginRename(id: string, nameEl: HTMLElement): void {
        const original = nameEl.textContent ?? "";
        nameEl.setAttribute("contenteditable", "true");
        nameEl.spellcheck = false;
        // Select all so typing replaces immediately.
        const sel = window.getSelection();
        const range = document.createRange();
        range.selectNodeContents(nameEl);
        sel?.removeAllRanges();
        sel?.addRange(range);
        nameEl.focus();

        const commit = (save: boolean) => {
            nameEl.removeAttribute("contenteditable");
            if (save) {
                this.opts.manager.rename(id, nameEl.textContent ?? original);
            } else {
                nameEl.textContent = original;
            }
            nameEl.removeEventListener("keydown", onKey);
            nameEl.removeEventListener("blur", onBlur);
        };

        const onKey = (e: KeyboardEvent) => {
            if (e.key === "Enter") {
                e.preventDefault();
                commit(true);
            } else if (e.key === "Escape") {
                e.preventDefault();
                commit(false);
            }
        };
        const onBlur = () => commit(true);

        nameEl.addEventListener("keydown", onKey);
        nameEl.addEventListener("blur", onBlur);
    }

    async closeSession(id: string): Promise<void> {
        const session = this.opts.manager.get(id);
        if (!session) return;
        if (session.state.running) {
            const ok = this.opts.confirmClose
                ? this.opts.confirmClose(session)
                : window.confirm(
                    `"${session.state.name}" is running a command. Close anyway?`,
                );
            if (!ok) return;
        }
        await this.opts.manager.close(id);
    }
}

function sessionMatchesFilter(session: Session, filter: string): boolean {
    const parts = [
        session.state.name,
        session.state.cwd ?? "",
        session.state.lastCommand ?? "",
        session.state.branch,
    ];
    return parts.some((p) => p.toLowerCase().includes(filter));
}

function shortenCwd(cwd: string): string {
    const home = /^\/Users\/[^/]+/;
    const withTilde = cwd.replace(home, "~");
    if (withTilde.length > 30) {
        return "…" + withTilde.slice(withTilde.length - 30);
    }
    return withTilde;
}

function escapeHtml(s: string): string {
    // SECURITY FIX (L-13): escape single-quote. Without it, this helper
    // is unsafe in single-quoted attribute contexts (`title='…'`) — a
    // future refactor that switches quoting styles silently regresses
    // to XSS. Matches the standard "big five" entities recommended by
    // OWASP's output-encoding cheat sheet.
    return s
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;")
        .replace(/'/g, "&#39;");
}

// --- Context menu (used by right-click on session rows) ----------------
//
// Lightweight inline implementation rather than a separate component:
// this is the only place ArcTerm uses a context menu today. If a second
// caller materializes (e.g. right-click on a block in the terminal),
// extract to its own file.

interface ContextMenuItem {
    label: string;
    /** Optional disabled state for items that aren't applicable in the
     *  current context (e.g. "Close Others" when only one session exists). */
    disabled?: boolean;
    action: () => void;
}

/**
 * Show a context menu at viewport coords. Only one menu can exist at a
 * time — opening a new one closes any previous instance first. Click
 * anywhere outside (mousedown), Esc, or selecting an item dismisses.
 */
function showContextMenu(
    clientX: number,
    clientY: number,
    items: ContextMenuItem[],
): void {
    // Tear down any existing menu first.
    document.querySelectorAll(".arcterm-context-menu").forEach((n) => n.remove());

    const menu = document.createElement("ul");
    menu.className = "arcterm-context-menu";
    menu.setAttribute("role", "menu");
    // We position offscreen first to measure, then nudge into the viewport
    // so the menu never spills off the right/bottom edges.
    menu.style.left = "-9999px";
    menu.style.top = "-9999px";

    for (const item of items) {
        const li = document.createElement("li");
        li.className = "arcterm-context-menu-item";
        if (item.disabled) li.classList.add("disabled");
        li.setAttribute("role", "menuitem");
        li.textContent = item.label;
        li.addEventListener("click", () => {
            if (item.disabled) return;
            close();
            item.action();
        });
        menu.append(li);
    }
    document.body.append(menu);

    // Nudge into viewport.
    const rect = menu.getBoundingClientRect();
    const x = Math.min(clientX, window.innerWidth - rect.width - 4);
    const y = Math.min(clientY, window.innerHeight - rect.height - 4);
    menu.style.left = `${Math.max(4, x)}px`;
    menu.style.top = `${Math.max(4, y)}px`;

    function close() {
        menu.remove();
        window.removeEventListener("mousedown", onOutside, true);
        window.removeEventListener("keydown", onKey, true);
    }
    function onOutside(ev: MouseEvent) {
        if (!menu.contains(ev.target as Node)) close();
    }
    function onKey(ev: KeyboardEvent) {
        if (ev.key === "Escape") {
            ev.preventDefault();
            close();
        }
    }
    // Capture-phase listeners so we win against everything else.
    window.addEventListener("mousedown", onOutside, true);
    window.addEventListener("keydown", onKey, true);
}
