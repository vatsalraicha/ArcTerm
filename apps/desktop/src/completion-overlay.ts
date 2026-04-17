/**
 * Tab-completion dropdown.
 *
 * Shown above the input editor when Tab produces multiple filesystem
 * matches. Single-match is handled inline by the editor (no dropdown
 * needed); this component only appears on disambiguation.
 *
 * Flow:
 *   - Editor intercepts Tab, calls `fs_complete` (Rust), renders the
 *     dropdown via `open(items, callback)`.
 *   - User picks with ↑/↓ + Enter/Tab, or clicks a row.
 *   - Callback receives the chosen replacement; the editor splices it in.
 *   - Escape dismisses without picking; Tab with no selection also closes.
 *
 * Interaction contract: while the dropdown is open, the editor's normal
 * keyboard handling is suspended for the nav keys (↑/↓/Enter/Esc/Tab) and
 * resumed on close. Everything else (typing, Cmd+←, etc.) still works —
 * typing narrows the visible items in-place without re-querying the FS
 * because in the common case the user refines rather than starts over.
 */

export interface CompletionItem {
    label: string;
    replacement: string;
    kind: "dir" | "file" | "executable";
    hidden: boolean;
}

export interface CompletionOverlayOptions {
    /** Element the overlay is appended into. Usually the input dock so it
     *  sits above the editor without covering the terminal output. */
    host: HTMLElement;
    /** Fires when the user commits a pick. */
    onPick: (item: CompletionItem) => void;
    /** Fires when the user cancels (Esc or clicks elsewhere). */
    onDismiss?: () => void;
}

export class CompletionOverlay {
    private readonly opts: CompletionOverlayOptions;
    private readonly root: HTMLDivElement;
    private items: CompletionItem[] = [];
    private visible: CompletionItem[] = [];
    private selectedIndex = 0;
    private open_ = false;
    private activeOnPick: (item: CompletionItem) => void;

    constructor(opts: CompletionOverlayOptions) {
        this.opts = opts;
        this.activeOnPick = opts.onPick;

        const root = document.createElement("div");
        root.className = "arcterm-completion-overlay hidden";
        root.setAttribute("role", "listbox");
        root.setAttribute("aria-label", "Tab completion");
        opts.host.append(root);
        this.root = root;

        root.addEventListener("mousedown", (ev) => {
            // Prevent the editor from losing focus — we want the caret to
            // stay put so the splice lands correctly.
            ev.preventDefault();
        });
        root.addEventListener("click", this.onClick);
    }

    isOpen(): boolean {
        return this.open_;
    }

    /**
     * Show the dropdown with the given items. First item is selected.
     * An optional per-call onPick overrides the constructor's default —
     * used by the editor to re-derive splice offsets at commit time from
     * the live caret (which may have moved since open()).
     */
    open(
        items: CompletionItem[],
        onPickOverride?: (item: CompletionItem) => void,
    ): void {
        if (items.length === 0) {
            this.close();
            return;
        }
        this.items = items;
        this.visible = items.slice();
        this.selectedIndex = 0;
        this.activeOnPick = onPickOverride ?? this.opts.onPick;
        this.open_ = true;
        this.root.classList.remove("hidden");
        this.renderList();
    }

    close(): void {
        if (!this.open_) return;
        this.open_ = false;
        this.root.classList.add("hidden");
        this.items = [];
        this.visible = [];
        this.opts.onDismiss?.();
    }

    /**
     * Return value tells the caller whether the event was consumed. Editors
     * should only apply their own default behavior when this returns false.
     */
    handleKey(ev: KeyboardEvent): boolean {
        if (!this.open_) return false;
        switch (ev.key) {
            case "ArrowDown":
                ev.preventDefault();
                this.move(1);
                return true;
            case "ArrowUp":
                ev.preventDefault();
                this.move(-1);
                return true;
            case "Enter":
            case "Tab":
                ev.preventDefault();
                this.commit();
                return true;
            case "Escape":
                ev.preventDefault();
                this.close();
                return true;
            default:
                return false;
        }
    }

    /**
     * Narrow visible items by a substring filter. Called by the editor on
     * keypress while the dropdown is open so the list shrinks as the user
     * types the disambiguating characters. We filter the in-memory list
     * rather than re-querying the FS because the user is refining an
     * already-listed set of entries.
     */
    filter(extraChars: string): void {
        if (!this.open_) return;
        if (!extraChars) {
            this.visible = this.items.slice();
        } else {
            const lc = extraChars.toLowerCase();
            this.visible = this.items.filter((i) =>
                i.label.toLowerCase().includes(lc),
            );
        }
        this.selectedIndex = 0;
        if (this.visible.length === 0) {
            // Nothing left — close rather than show an empty box.
            this.close();
            return;
        }
        this.renderList();
    }

    // -- internals ---------------------------------------------------------

    private renderList(): void {
        this.root.innerHTML = "";
        for (let i = 0; i < this.visible.length; i++) {
            const item = this.visible[i];
            const row = document.createElement("div");
            row.className = "arcterm-completion-item";
            if (i === this.selectedIndex) row.classList.add("selected");
            if (item.hidden) row.classList.add("hidden-entry");
            row.dataset.index = String(i);
            row.setAttribute("role", "option");
            row.setAttribute("aria-selected", i === this.selectedIndex ? "true" : "false");

            const icon = document.createElement("span");
            icon.className = "arcterm-completion-icon";
            icon.textContent =
                item.kind === "dir" ? "📁"
                : item.kind === "executable" ? "⚡"
                : "📄";
            icon.setAttribute("aria-hidden", "true");

            const label = document.createElement("span");
            label.className = "arcterm-completion-label";
            label.textContent = item.label;

            const meta = document.createElement("span");
            meta.className = "arcterm-completion-meta";
            meta.textContent =
                item.kind === "dir" ? "Directory"
                : item.kind === "executable" ? "Executable"
                : "File";

            row.append(icon, label, meta);
            this.root.append(row);
        }
        this.scrollSelectionIntoView();
    }

    private move(delta: number): void {
        if (this.visible.length === 0) return;
        this.selectedIndex =
            (this.selectedIndex + delta + this.visible.length) % this.visible.length;
        const rows = this.root.querySelectorAll(".arcterm-completion-item");
        rows.forEach((el, i) => {
            el.classList.toggle("selected", i === this.selectedIndex);
            el.setAttribute("aria-selected", i === this.selectedIndex ? "true" : "false");
        });
        this.scrollSelectionIntoView();
    }

    private commit(): void {
        const item = this.visible[this.selectedIndex];
        if (!item) return;
        const onPick = this.activeOnPick;
        this.close();
        onPick(item);
    }

    private scrollSelectionIntoView(): void {
        const rows = this.root.querySelectorAll(".arcterm-completion-item");
        const row = rows[this.selectedIndex] as HTMLElement | undefined;
        row?.scrollIntoView({ block: "nearest" });
    }

    private readonly onClick = (ev: MouseEvent): void => {
        const row = (ev.target as HTMLElement).closest(
            ".arcterm-completion-item",
        ) as HTMLElement | null;
        if (!row) return;
        const i = Number.parseInt(row.dataset.index ?? "-1", 10);
        if (i < 0 || i >= this.visible.length) return;
        this.selectedIndex = i;
        this.commit();
    };
}
