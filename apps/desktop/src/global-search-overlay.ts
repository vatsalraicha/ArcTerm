/**
 * Global search overlay (⌘⇧F).
 *
 * Searches across two corpora at once:
 *   1. Command history — backed by the SQLite history DB via the existing
 *      `history_search` IPC. No cwd boost: this is global by definition.
 *   2. Live session scrollback — every open session's xterm "normal"
 *      buffer is grepped via `TerminalHandle.searchBuffer`. Matches are
 *      jump-to-able: selecting one switches to the session and scrolls
 *      its viewport so the matching line is visible.
 *
 * The two result kinds share one keyboard-navigable list. Commands are
 * listed first (most relevant — the user typed them, the shell remembers
 * them), buffer hits second (raw output that scrolled past). De-duping
 * is intentionally NOT done: a `git status` hit in history shows ✓/cwd
 * context; the same line in a buffer shows session + scroll-position
 * context. Keeping both gives the user two ways to act on it.
 *
 * Design parallels history-overlay.ts deliberately: the user already
 * knows that overlay's keys (↑↓ navigate, Enter select, Esc dismiss).
 * Reusing the visual language avoids surprising them.
 */

import { invoke } from "@tauri-apps/api/core";

import type { HistoryEntry } from "./history-overlay";
import type { SessionManager } from "./session-manager";

export interface GlobalSearchOverlayOptions {
    /** Parent element (typically `#app`). The overlay mounts itself on
     *  construction so opening is just a class toggle. */
    host: HTMLElement;
    /** SessionManager — used to enumerate sessions for buffer search and
     *  to switch to the chosen session on jump. */
    manager: SessionManager;
    /** Fires when the user picks a command-kind hit. The caller (main.ts)
     *  populates the input editor with it (same UX as history overlay). */
    onPickCommand: (command: string) => void;
    /** Fires when the user picks a buffer-kind hit. Caller switches to
     *  the session, scrolls the viewport, and refocuses the editor. */
    onPickBuffer: (sessionId: string, absLine: number) => void;
    /** Fires when the overlay closes without a selection. */
    onDismiss?: () => void;
}

/** Per-corpus result caps. Keeping them tight avoids dumping a giant DOM
 *  for short queries that match many lines. The history cap mirrors the
 *  existing history overlay; buffer cap is per-session. */
const MAX_HISTORY_RESULTS = 30;
const MAX_BUFFER_RESULTS_PER_SESSION = 20;

/** Minimum query length before we run a search. 2 keeps single-character
 *  typos from yielding pages of false matches in long scrollbacks. */
const MIN_QUERY_LEN = 2;

type Hit =
    | { kind: "command"; entry: HistoryEntry }
    | {
          kind: "buffer";
          sessionId: string;
          sessionName: string;
          absLine: number;
          text: string;
      };

export class GlobalSearchOverlay {
    private readonly opts: GlobalSearchOverlayOptions;
    private readonly root: HTMLDivElement;
    private readonly searchInput: HTMLInputElement;
    private readonly list: HTMLUListElement;
    private readonly counts: HTMLDivElement;
    private hits: Hit[] = [];
    private selectedIndex = 0;
    private searchSeq = 0;
    private open_ = false;

    constructor(opts: GlobalSearchOverlayOptions) {
        this.opts = opts;

        // Build the DOM once on construction; opening is just a class
        // toggle. Same lifetime model as history-overlay.
        const root = document.createElement("div");
        root.className = "arcterm-global-search-overlay hidden";
        root.setAttribute("role", "dialog");
        root.setAttribute("aria-label", "Global search");
        root.setAttribute("aria-modal", "true");
        root.tabIndex = -1;

        const panel = document.createElement("div");
        panel.className = "arcterm-global-search-panel";

        const searchRow = document.createElement("div");
        searchRow.className = "arcterm-global-search-row";
        const searchIcon = document.createElement("span");
        searchIcon.className = "arcterm-global-search-icon";
        searchIcon.textContent = "⌕";
        searchIcon.setAttribute("aria-hidden", "true");
        const searchInput = document.createElement("input");
        searchInput.type = "text";
        searchInput.placeholder =
            "Search commands and session output…";
        searchInput.spellcheck = false;
        searchInput.setAttribute("autocorrect", "off");
        searchInput.setAttribute("autocapitalize", "off");
        searchRow.append(searchIcon, searchInput);

        const counts = document.createElement("div");
        counts.className = "arcterm-global-search-counts";
        counts.textContent = "Type to search.";

        const list = document.createElement("ul");
        list.className = "arcterm-global-search-list";

        const footer = document.createElement("div");
        footer.className = "arcterm-global-search-footer";
        footer.textContent =
            "↑↓ navigate   Enter select   Esc dismiss";

        panel.append(searchRow, counts, list, footer);
        root.append(panel);
        opts.host.append(root);

        this.root = root;
        this.searchInput = searchInput;
        this.list = list;
        this.counts = counts;

        // Debounce keystroke -> search by 80 ms. Buffer scans are cheap
        // enough (10k lines * N sessions = a few ms) that we can afford
        // to be aggressive, but coalescing rapid typing avoids redundant
        // work and keeps the result list from flickering character-by-
        // character.
        let inputTimer: number | undefined;
        searchInput.addEventListener("input", () => {
            window.clearTimeout(inputTimer);
            inputTimer = window.setTimeout(() => {
                void this.refresh(searchInput.value);
            }, 80);
        });
        root.addEventListener("keydown", this.onKeyDown);
        // Click on the scrim (root, not the panel) closes.
        root.addEventListener("mousedown", (ev) => {
            if (ev.target === root) this.close();
        });
        list.addEventListener("click", this.onListClick);
    }

    isOpen(): boolean {
        return this.open_;
    }

    open(): void {
        if (this.open_) return;
        this.open_ = true;
        this.root.classList.remove("hidden");
        this.searchInput.value = "";
        this.hits = [];
        this.selectedIndex = 0;
        this.counts.textContent = "Type to search.";
        this.renderList();
        this.searchInput.focus();
    }

    close(): void {
        if (!this.open_) return;
        this.open_ = false;
        this.root.classList.add("hidden");
        this.opts.onDismiss?.();
    }

    // -- internals -------------------------------------------------------

    private async refresh(rawQuery: string): Promise<void> {
        const query = rawQuery.trim();
        const seq = ++this.searchSeq;

        if (query.length < MIN_QUERY_LEN) {
            this.hits = [];
            this.selectedIndex = 0;
            this.counts.textContent =
                query.length === 0
                    ? "Type to search."
                    : `Keep typing… (${MIN_QUERY_LEN}+ chars)`;
            this.renderList();
            return;
        }

        // 1. Buffer search runs synchronously off the live xterm
        //    instances - no IPC. We do this before the await on
        //    history_search so the UI fills in for the buffer half
        //    even if the history DB is slow.
        const bufferHits: Hit[] = [];
        for (const session of this.opts.manager.list()) {
            const matches = session.terminal.searchBuffer(
                query,
                MAX_BUFFER_RESULTS_PER_SESSION,
            );
            for (const m of matches) {
                bufferHits.push({
                    kind: "buffer",
                    sessionId: session.id,
                    sessionName: session.state.name,
                    absLine: m.absLine,
                    text: m.text,
                });
            }
        }

        // 2. History search via IPC. Pass null cwd so this is a TRUE
        //    global search (the existing history overlay boosts in-cwd
        //    results - that's not what users want from Cmd+Shift+F).
        let historyHits: Hit[] = [];
        try {
            const rows = await invoke<HistoryEntry[]>("history_search", {
                query,
                cwd: null,
                limit: MAX_HISTORY_RESULTS,
            });
            // Stale-response guard: if the user typed more characters
            // while we were awaiting, this response is no longer
            // current.
            if (seq !== this.searchSeq) return;
            historyHits = rows.map((entry) => ({
                kind: "command",
                entry,
            }));
        } catch (err) {
            console.error("history_search failed in global search", err);
        }

        // Same stale-response guard for the buffer-only path (in case
        // history_search rejected synchronously and we skipped the
        // await above).
        if (seq !== this.searchSeq) return;

        // Commands first (typically more relevant since the user typed
        // them by hand), then buffer matches.
        this.hits = [...historyHits, ...bufferHits];
        this.selectedIndex = 0;
        this.counts.textContent = this.formatCounts(
            historyHits.length,
            bufferHits.length,
        );
        this.renderList();
    }

    private formatCounts(commandCount: number, bufferCount: number): string {
        if (commandCount === 0 && bufferCount === 0) {
            return "No matches.";
        }
        const cmdLabel = commandCount === 1 ? "command" : "commands";
        const bufLabel = bufferCount === 1 ? "match" : "matches";
        return `${commandCount} ${cmdLabel} · ${bufferCount} buffer ${bufLabel}`;
    }

    private renderList(): void {
        this.list.innerHTML = "";
        if (this.hits.length === 0) {
            return;
        }

        let lastSection: "command" | "buffer" | null = null;
        this.hits.forEach((hit, i) => {
            // Section divider when the kind changes - purely visual.
            if (hit.kind !== lastSection) {
                const header = document.createElement("li");
                header.className = "arcterm-global-search-section";
                header.textContent =
                    hit.kind === "command" ? "Commands" : "Output";
                this.list.append(header);
                lastSection = hit.kind;
            }

            const li = document.createElement("li");
            li.className = "arcterm-global-search-item";
            if (i === this.selectedIndex) li.classList.add("selected");
            li.dataset.index = String(i);

            if (hit.kind === "command") {
                const cmd = document.createElement("span");
                cmd.className = "arcterm-global-search-cmd";
                cmd.textContent = hit.entry.command;
                const meta = document.createElement("span");
                meta.className = "arcterm-global-search-meta";
                if (hit.entry.exit_code === 0) {
                    meta.append(badge("arcterm-global-search-ok", "✓"));
                } else if (
                    hit.entry.exit_code !== null &&
                    hit.entry.exit_code !== 0
                ) {
                    meta.append(
                        badge(
                            "arcterm-global-search-err",
                            `✗ ${hit.entry.exit_code}`,
                        ),
                    );
                }
                if (hit.entry.cwd) {
                    meta.append(
                        badge(
                            "arcterm-global-search-cwd",
                            shortenCwd(hit.entry.cwd),
                        ),
                    );
                }
                li.append(cmd, meta);
            } else {
                const text = document.createElement("span");
                text.className = "arcterm-global-search-cmd";
                text.textContent = hit.text;
                const meta = document.createElement("span");
                meta.className = "arcterm-global-search-meta";
                meta.append(
                    badge(
                        "arcterm-global-search-session",
                        hit.sessionName,
                    ),
                );
                li.append(text, meta);
            }

            this.list.append(li);
        });
        this.scrollSelectionIntoView();
    }

    private readonly onListClick = (ev: MouseEvent): void => {
        const target = (ev.target as HTMLElement).closest(
            ".arcterm-global-search-item",
        ) as HTMLElement | null;
        if (!target) return;
        const i = Number.parseInt(target.dataset.index ?? "-1", 10);
        if (i >= 0 && i < this.hits.length) {
            this.select(i);
        }
    };

    private readonly onKeyDown = (ev: KeyboardEvent): void => {
        if (!this.open_) return;
        if (ev.key === "Escape") {
            ev.preventDefault();
            this.close();
            return;
        }
        if (ev.key === "Enter") {
            ev.preventDefault();
            if (this.hits.length > 0) this.select(this.selectedIndex);
            return;
        }
        if (ev.key === "ArrowUp") {
            ev.preventDefault();
            this.move(-1);
            return;
        }
        if (ev.key === "ArrowDown") {
            ev.preventDefault();
            this.move(1);
            return;
        }
    };

    private move(delta: number): void {
        if (this.hits.length === 0) return;
        const next = this.selectedIndex + delta;
        this.selectedIndex = Math.max(
            0,
            Math.min(this.hits.length - 1, next),
        );
        // Re-tag the selected row without rebuilding the list.
        const items = this.list.querySelectorAll(
            ".arcterm-global-search-item",
        );
        items.forEach((el, i) => {
            el.classList.toggle("selected", i === this.selectedIndex);
        });
        this.scrollSelectionIntoView();
    }

    private select(i: number): void {
        const hit = this.hits[i];
        if (!hit) return;
        this.close();
        if (hit.kind === "command") {
            this.opts.onPickCommand(hit.entry.command);
        } else {
            this.opts.onPickBuffer(hit.sessionId, hit.absLine);
        }
    }

    private scrollSelectionIntoView(): void {
        const items = this.list.querySelectorAll(
            ".arcterm-global-search-item",
        );
        const item = items[this.selectedIndex] as HTMLElement | undefined;
        item?.scrollIntoView({ block: "nearest" });
    }
}

function shortenCwd(cwd: string): string {
    // Same defense-in-depth as history-overlay: strip ASCII control
    // bytes and Unicode line separators / bidi overrides that history
    // sanitation should already prevent at write time.
    // eslint-disable-next-line no-control-regex
    const clean = cwd.replace(
        /[\x00-\x1f\x7f\u0085\u2028\u2029\u202A-\u202E\u2066-\u2069]/g,
        "",
    );
    const home = "/Users/";
    if (clean.length > 40) {
        return "…" + clean.slice(clean.length - 40);
    }
    return clean.replace(new RegExp(`^${home}[^/]+`), "~");
}

function badge(className: string, text: string): HTMLSpanElement {
    const el = document.createElement("span");
    el.className = className;
    el.textContent = text;
    return el;
}
