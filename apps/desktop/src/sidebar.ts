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
    return s
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;");
}
