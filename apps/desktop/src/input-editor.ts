/**
 * ArcTerm's custom input editor.
 *
 * A contenteditable div pinned at the bottom of the window. The entire
 * point of the widget is that typing here feels like a code editor, not
 * a VT100 line buffer — full caret, multi-line, macOS keybindings, and
 * (Phase 3) ghost-text autocomplete and an Up-arrow history overlay.
 *
 * Why contenteditable and not a textarea?
 *   - A textarea can't show mixed styles (typed text normal, ghost text
 *     dimmed) in the same line. We render ghost text as a child span
 *     with `contenteditable=false` so the caret skips over it naturally.
 *   - WebKit handles most macOS keybindings natively inside contenteditable
 *     — Cmd+←, Option+←, selections, clipboard — so we only intercept the
 *     commands that mean something ArcTerm-specific (Enter, Tab, ↑/↓, Esc,
 *     Ctrl+C).
 *
 * Ghost-text protocol (Phase 3):
 *   1. On every keystroke that changes the text, we debounce 50ms and ask
 *      the caller's `suggestFor(prefix)` for the best completion.
 *   2. The returned suffix is appended as a non-editable span after the
 *      user's typed text. Because it lives inside .arcterm-input-editor,
 *      it's styled dim; because it's contenteditable=false, the caret
 *      can't enter it and Shift+→ selections stop at its left edge.
 *   3. `→` at the end of input accepts the suggestion (replaces ghost
 *      span with real text); `Tab` same; any typed character replaces it.
 */

export interface InputEditorOptions {
    host: HTMLElement;
    /** Fired when Enter is pressed. Receives the full text (can be multi-line). */
    onSubmit: (command: string) => void;
    /** Fired on Ctrl+C. Caller decides whether to forward as SIGINT. */
    onInterrupt?: () => void;
    /** Fired on ↑ — Phase 3 opens the history overlay. */
    onHistoryUp?: () => void;
    /** Fired on ↓ — reserved for Phase 3 history overlay navigation. */
    onHistoryDown?: () => void;
    /** Fired on Ctrl+R — opens history overlay in search mode. */
    onSearchHistory?: () => void;
    /**
     * Look up an autosuggest completion for the given prefix. Return the
     * *suffix* to show (i.e. what follows the prefix), not the full command.
     * Returning null/empty string hides the ghost.
     */
    suggestFor?: (prefix: string) => Promise<string | null>;
}

const GHOST_CLASS = "arcterm-ghost";
const SUGGEST_DEBOUNCE_MS = 50;

export class InputEditor {
    private readonly el: HTMLDivElement;
    private readonly opts: InputEditorOptions;
    private suggestTimer: number | undefined;
    private suggestSeq = 0; // monotonic — discard stale async results

    constructor(opts: InputEditorOptions) {
        this.opts = opts;

        const el = document.createElement("div");
        el.className = "arcterm-input-editor";
        el.contentEditable = "true";
        el.spellcheck = false;
        el.setAttribute("autocorrect", "off");
        el.setAttribute("autocapitalize", "off");
        el.setAttribute("aria-label", "ArcTerm command input");
        el.setAttribute("role", "textbox");
        el.setAttribute("aria-multiline", "true");
        this.el = el;

        opts.host.appendChild(el);
        this.attachEventHandlers();

        // Focus on mount so the user can start typing immediately.
        requestAnimationFrame(() => el.focus());
    }

    /** Current editor contents (excluding ghost-text span), newlines preserved. */
    getValue(): string {
        // Clone so we can strip the ghost span without mutating the live DOM.
        const clone = this.el.cloneNode(true) as HTMLDivElement;
        clone.querySelectorAll(`.${GHOST_CLASS}`).forEach((n) => n.remove());
        return clone.innerText.replace(/\u00a0/g, " ");
    }

    /** Replace editor contents. Cursor lands at the end. */
    setValue(text: string): void {
        this.removeGhost();
        this.el.textContent = text;
        this.moveCursorToEnd();
        this.scheduleSuggest();
    }

    /** Empty the editor. */
    clear(): void {
        this.removeGhost();
        this.el.textContent = "";
    }

    /** Grab focus. Called when the app regains window focus. */
    focus(): void {
        this.el.focus();
    }

    // -- internals -----------------------------------------------------------

    private attachEventHandlers(): void {
        this.el.addEventListener("keydown", this.onKeyDown);
        this.el.addEventListener("input", this.onInput);
        this.el.addEventListener("paste", this.onPaste);
    }

    private readonly onKeyDown = (ev: KeyboardEvent): void => {
        // --- Submission keys ---
        if (ev.key === "Enter") {
            if (ev.shiftKey) {
                // Shift+Enter = newline. WebKit's default behavior on <div>
                // contenteditable is to insert a new <div>, which survives
                // innerText correctly. We let the browser handle it rather
                // than re-implementing line insertion at the caret.
                // Also strip any visible ghost first so the suggestion
                // doesn't end up accidentally committed mid-line.
                this.removeGhost();
                return;
            }
            // Plain Enter = submit.
            ev.preventDefault();
            this.removeGhost();
            const text = this.getValue();
            this.opts.onSubmit(text);
            this.clear();
            return;
        }

        // --- Ctrl+C: forward to PTY as SIGINT ---
        if (ev.ctrlKey && !ev.metaKey && !ev.altKey && ev.key === "c") {
            ev.preventDefault();
            const sel = window.getSelection();
            if (sel && sel.toString().length > 0) {
                return; // let default copy proceed
            }
            this.opts.onInterrupt?.();
            this.clear();
            return;
        }

        // --- Ctrl+R: search history ---
        if (ev.ctrlKey && !ev.metaKey && !ev.altKey && ev.key === "r") {
            ev.preventDefault();
            this.opts.onSearchHistory?.();
            return;
        }

        // --- Tab: accept autosuggestion if present ---
        if (ev.key === "Tab") {
            ev.preventDefault();
            if (!this.acceptGhost()) {
                // No ghost to accept — reserved for tab completion in a
                // future phase. Swallow so focus doesn't escape the editor.
            }
            return;
        }

        // --- Arrow right at end of line: accept ghost ---
        if (ev.key === "ArrowRight" && this.caretAtEnd() && !ev.shiftKey) {
            if (this.acceptGhost()) {
                ev.preventDefault();
                return;
            }
            // No ghost, fall through to native caret motion.
        }

        // --- ↑/↓: history overlay hooks (Phase 3) ---
        // We swallow these so they don't move the caret into multi-line
        // editing mode unexpectedly.
        if (ev.key === "ArrowUp") {
            ev.preventDefault();
            this.opts.onHistoryUp?.();
            return;
        }
        if (ev.key === "ArrowDown") {
            ev.preventDefault();
            this.opts.onHistoryDown?.();
            return;
        }

        // --- Escape: clear input ---
        if (ev.key === "Escape") {
            ev.preventDefault();
            this.clear();
            return;
        }

        // Everything else — Cmd+←, Option+←, Cmd+A, Cmd+C/V/X on selection,
        // normal typing — falls through to WebKit's built-in handling.
    };

    /** Fires after any content mutation. We use it to refresh the ghost. */
    private readonly onInput = (): void => {
        // Any typed char invalidates the current ghost. Remove immediately
        // (don't wait for the debounce) so a stale suggestion can't flash.
        this.removeGhost();
        this.scheduleSuggest();
    };

    /**
     * Intercept paste to insert plain text only. Without this, pasting from
     * a browser or rich-text app drops HTML nodes into the editor, which
     * both looks wrong and breaks `getValue()` round-trips.
     */
    private readonly onPaste = (ev: ClipboardEvent): void => {
        ev.preventDefault();
        const text = ev.clipboardData?.getData("text/plain") ?? "";
        if (!text) return;
        // execCommand is deprecated but it's still the only reliable way to
        // insert text at the caret while preserving undo history in
        // contenteditable. We'll swap to the Input Events API later if WebKit
        // ever removes it.
        document.execCommand("insertText", false, text);
    };

    // --- Suggestion plumbing ---------------------------------------------

    private scheduleSuggest(): void {
        if (!this.opts.suggestFor) return;
        window.clearTimeout(this.suggestTimer);
        this.suggestTimer = window.setTimeout(() => this.refreshSuggest(), SUGGEST_DEBOUNCE_MS);
    }

    private async refreshSuggest(): Promise<void> {
        const fn = this.opts.suggestFor;
        if (!fn) return;

        const prefix = this.getValue();
        // Monotonic sequence so a slow earlier query that resolves after a
        // later one can't overwrite the newer suggestion.
        const seq = ++this.suggestSeq;

        // Don't suggest when empty or when caret isn't at end of input —
        // ghost text in the middle of a line is confusing.
        if (!prefix || !this.caretAtEnd()) {
            this.removeGhost();
            return;
        }
        let suffix: string | null = null;
        try {
            suffix = await fn(prefix);
        } catch (err) {
            console.error("suggestFor failed", err);
            return;
        }
        if (seq !== this.suggestSeq) return; // stale
        if (!suffix) {
            this.removeGhost();
            return;
        }
        // Drop suggestions that contain a newline — multi-line ghost text
        // would shift the editor height on every keystroke, which feels bad.
        if (suffix.includes("\n")) return;
        this.renderGhost(suffix);
    }

    private renderGhost(suffix: string): void {
        this.removeGhost();
        const span = document.createElement("span");
        span.className = GHOST_CLASS;
        span.contentEditable = "false";
        // Use a data attribute for the text so CSS can ::before it if we
        // later want to restyle without re-rendering. For now, textContent.
        span.textContent = suffix;
        this.el.appendChild(span);
        // Make sure caret doesn't drift into the span. It shouldn't
        // (contenteditable=false blocks it) but a stray click could try.
    }

    private removeGhost(): void {
        this.el.querySelectorAll(`.${GHOST_CLASS}`).forEach((n) => n.remove());
    }

    /**
     * Accept the current ghost: replace the span with real text. Return
     * true if there was a ghost to accept.
     */
    private acceptGhost(): boolean {
        const ghost = this.el.querySelector(`.${GHOST_CLASS}`);
        if (!ghost) return false;
        const text = ghost.textContent ?? "";
        ghost.remove();
        // insertText places the string at the caret and respects undo.
        // Before inserting, make sure the caret is at the end (it should be,
        // but a click could've moved it).
        this.moveCursorToEnd();
        document.execCommand("insertText", false, text);
        return true;
    }

    /** Is the caret at the very end of the editable content (excluding ghost)? */
    private caretAtEnd(): boolean {
        const sel = window.getSelection();
        if (!sel || sel.rangeCount === 0) return true;
        const range = sel.getRangeAt(0);
        if (!range.collapsed) return false;
        // Walk forward from the caret to the editor end; if only ghost
        // spans lie between, we count as "at end" for ghost-accept logic.
        let node: Node | null = range.endContainer;
        let offset = range.endOffset;
        while (node) {
            if (node.nodeType === Node.TEXT_NODE) {
                if (offset < (node.nodeValue ?? "").length) return false;
            } else if (node instanceof Element) {
                // Walk into child nodes beyond the end offset.
                for (let i = offset; i < node.childNodes.length; i++) {
                    const child = node.childNodes[i];
                    if (
                        child instanceof Element &&
                        child.classList.contains(GHOST_CLASS)
                    ) {
                        continue;
                    }
                    // Any non-ghost node after the caret means we're not at end.
                    if (child.textContent && child.textContent.length > 0) {
                        return false;
                    }
                }
            }
            // Climb to next sibling or ancestor.
            if (node === this.el) break;
            if (node.parentNode && node.parentNode !== this.el) {
                const parent = node.parentNode;
                offset =
                    Array.prototype.indexOf.call(parent.childNodes, node) + 1;
                node = parent;
            } else {
                break;
            }
        }
        return true;
    }

    private moveCursorToEnd(): void {
        const range = document.createRange();
        range.selectNodeContents(this.el);
        range.collapse(false);
        const sel = window.getSelection();
        if (sel) {
            sel.removeAllRanges();
            sel.addRange(range);
        }
    }
}
