/**
 * Visual command-history browser.
 *
 * Opens above the input editor when the user presses ↑ (most recent first)
 * or Ctrl+R (focus the search input first). Behavior mirrors zsh's
 * reverse-i-search but with a proper scrollable list instead of a one-
 * result-at-a-time cycle.
 *
 * Lifecycle:
 *   - `open(mode)` mounts the DOM and loads initial results.
 *   - Arrow keys navigate, Enter selects, Esc dismisses.
 *   - `onSelect(command)` fires with the chosen command — the caller
 *     (main.ts) populates the input editor with it.
 *
 * The overlay steals focus while open; closing returns focus to the
 * input editor (the caller re-focuses via InputEditor.focus()).
 */

import { invoke } from "@tauri-apps/api/core";

export interface HistoryEntry {
    id: number;
    command: string;
    cwd: string | null;
    exit_code: number | null;
    started_at: number;
    duration_ms: number | null;
}

export interface HistoryOverlayOptions {
    /** Parent element to append the overlay into (usually the app root). */
    host: HTMLElement;
    /**
     * Fires when the user picks a command. Overlay closes automatically
     * after this fires.
     */
    onSelect: (command: string) => void;
    /** Fires when the overlay closes without a selection. */
    onDismiss?: () => void;
    /** Current working directory — used to boost in-cwd results in search. */
    getCwd?: () => string | null;
}

const MAX_RESULTS = 60;

export class HistoryOverlay {
    private readonly opts: HistoryOverlayOptions;
    private readonly root: HTMLDivElement;
    private readonly searchInput: HTMLInputElement;
    private readonly list: HTMLUListElement;
    private entries: HistoryEntry[] = [];
    private selectedIndex = 0;
    private searchSeq = 0;
    private open_ = false;

    constructor(opts: HistoryOverlayOptions) {
        this.opts = opts;

        // Build the overlay. We keep it permanently in the DOM (hidden when
        // closed) so we don't pay mount/unmount cost every time the user
        // presses ↑. The backdrop sits above the terminal frame but below
        // any settings/modal we might add later.
        const root = document.createElement("div");
        root.className = "arcterm-history-overlay hidden";
        root.setAttribute("role", "dialog");
        root.setAttribute("aria-label", "Command history");
        root.setAttribute("aria-modal", "true");
        // tabindex=-1 makes the div focusable programmatically without
        // appearing in the tab order. Needed so "browse" mode can receive
        // arrow-key events before routing typing to the search input.
        root.tabIndex = -1;

        const panel = document.createElement("div");
        panel.className = "arcterm-history-panel";

        const searchRow = document.createElement("div");
        searchRow.className = "arcterm-history-search";
        const searchIcon = document.createElement("span");
        searchIcon.className = "arcterm-history-search-icon";
        searchIcon.textContent = "⌕";
        searchIcon.setAttribute("aria-hidden", "true");
        const searchInput = document.createElement("input");
        searchInput.type = "text";
        searchInput.placeholder = "Search command history…";
        searchInput.spellcheck = false;
        searchInput.setAttribute("autocorrect", "off");
        searchInput.setAttribute("autocapitalize", "off");
        searchRow.append(searchIcon, searchInput);

        const list = document.createElement("ul");
        list.className = "arcterm-history-list";

        const footer = document.createElement("div");
        footer.className = "arcterm-history-footer";
        footer.textContent = "↑↓ navigate   Enter select   Esc dismiss";

        panel.append(searchRow, list, footer);
        root.append(panel);
        opts.host.append(root);

        this.root = root;
        this.searchInput = searchInput;
        this.list = list;

        searchInput.addEventListener("input", () => this.refresh(searchInput.value));
        root.addEventListener("keydown", this.onKeyDown);
        // Clicking outside the panel dismisses. Clicks inside bubble up but
        // we check target to avoid dismissing on panel-internal clicks.
        root.addEventListener("mousedown", (ev) => {
            if (ev.target === root) this.close();
        });
        list.addEventListener("click", this.onListClick);
    }

    /** True when the overlay is currently visible. */
    isOpen(): boolean {
        return this.open_;
    }

    /**
     * Show the overlay. `mode` controls initial focus:
     *   - "browse": focus the list (user arrived via ↑)
     *   - "search": focus the search input (user arrived via Ctrl+R)
     * In both cases the search field starts empty and shows recent entries.
     */
    async open(mode: "browse" | "search"): Promise<void> {
        if (this.open_) return;
        this.open_ = true;
        this.root.classList.remove("hidden");
        this.searchInput.value = "";
        await this.refresh("");
        if (mode === "search") {
            this.searchInput.focus();
        } else {
            // Focus the list container so arrow keys fire on keydown handler.
            this.root.focus();
            // A div isn't focusable by default; tabindex=-1 set in CSS makes it so.
        }
    }

    close(): void {
        if (!this.open_) return;
        this.open_ = false;
        this.root.classList.add("hidden");
        this.opts.onDismiss?.();
    }

    // -- internals -----------------------------------------------------------

    private async refresh(query: string): Promise<void> {
        const seq = ++this.searchSeq;
        const cwd = this.opts.getCwd?.() ?? null;
        try {
            const results = await invoke<HistoryEntry[]>("history_search", {
                query,
                cwd,
                limit: MAX_RESULTS,
            });
            if (seq !== this.searchSeq) return; // stale
            this.entries = results;
            this.selectedIndex = 0;
            this.renderList();
        } catch (err) {
            console.error("history_search failed", err);
            this.entries = [];
            this.renderList();
        }
    }

    private renderList(): void {
        // Rebuild children. History lists are small (≤60) so full re-render
        // is cheaper than diffing.
        this.list.innerHTML = "";
        if (this.entries.length === 0) {
            const empty = document.createElement("li");
            empty.className = "arcterm-history-empty";
            empty.textContent = "No matching history yet.";
            this.list.append(empty);
            return;
        }
        this.entries.forEach((entry, i) => {
            const li = document.createElement("li");
            li.className = "arcterm-history-item";
            if (i === this.selectedIndex) li.classList.add("selected");
            li.dataset.index = String(i);

            const cmd = document.createElement("span");
            cmd.className = "arcterm-history-cmd";
            cmd.textContent = entry.command;

            const meta = document.createElement("span");
            meta.className = "arcterm-history-meta";
            // SECURITY FIX: avoid innerHTML entirely. Previous version
            // interpolated exit_code + escaped cwd into HTML strings; a
            // future schema change making exit_code a string would lose
            // all sanitization. Use DOM nodes with textContent so there
            // is no string-to-HTML boundary to guard.
            if (entry.exit_code === 0) {
                meta.append(badge("arcterm-history-ok", "✓"));
                meta.append(document.createTextNode("  "));
            } else if (entry.exit_code && entry.exit_code !== 0) {
                meta.append(
                    badge("arcterm-history-err", `✗ ${entry.exit_code}`),
                );
                meta.append(document.createTextNode("  "));
            }
            if (entry.cwd) {
                meta.append(
                    badge("arcterm-history-cwd", shortenCwd(entry.cwd)),
                );
            }

            li.append(cmd, meta);
            this.list.append(li);
        });
        this.scrollSelectionIntoView();
    }

    private readonly onListClick = (ev: MouseEvent): void => {
        const target = (ev.target as HTMLElement).closest(
            ".arcterm-history-item",
        ) as HTMLElement | null;
        if (!target) return;
        const i = Number.parseInt(target.dataset.index ?? "-1", 10);
        if (i >= 0 && i < this.entries.length) {
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
            if (this.entries.length > 0) this.select(this.selectedIndex);
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
        // Any other key while focus is elsewhere: route into search box.
        if (
            document.activeElement !== this.searchInput &&
            ev.key.length === 1 &&
            !ev.metaKey &&
            !ev.ctrlKey
        ) {
            this.searchInput.focus();
            // Don't preventDefault — let the char land in the input.
        }
    };

    private move(delta: number): void {
        if (this.entries.length === 0) return;
        const next = this.selectedIndex + delta;
        this.selectedIndex = Math.max(0, Math.min(this.entries.length - 1, next));
        // Update selection classes without re-rendering everything.
        const items = this.list.querySelectorAll(".arcterm-history-item");
        items.forEach((el, i) => {
            el.classList.toggle("selected", i === this.selectedIndex);
        });
        this.scrollSelectionIntoView();
    }

    private select(i: number): void {
        const entry = this.entries[i];
        if (!entry) return;
        this.close();
        this.opts.onSelect(entry.command);
    }

    private scrollSelectionIntoView(): void {
        const item = this.list.querySelectorAll(".arcterm-history-item")[
            this.selectedIndex
        ] as HTMLElement | undefined;
        item?.scrollIntoView({ block: "nearest" });
    }
}

function shortenCwd(cwd: string): string {
    const home = "/Users/"; // rough match; display-only, exact HOME not critical
    if (cwd.length > 40) {
        return "…" + cwd.slice(cwd.length - 40);
    }
    return cwd.replace(new RegExp(`^${home}[^/]+`), "~");
}

function badge(className: string, text: string): HTMLSpanElement {
    const el = document.createElement("span");
    el.className = className;
    el.textContent = text;
    return el;
}
