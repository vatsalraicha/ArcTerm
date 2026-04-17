/**
 * ArcTerm's custom input editor.
 *
 * A contenteditable div pinned at the bottom of the window. Modern editor
 * ergonomics (Cmd+← line-jump, Option+← word-jump, multi-line Shift+Enter,
 * standard text selection) — the entire point of the widget is that typing
 * here feels like a code editor, not a VT100 line buffer.
 *
 * Why contenteditable and not a textarea?
 *   - textarea forces a single monospace font metric and limited styling;
 *     we need inline decorations (ghost text in Phase 3, token highlights,
 *     later command-argument hints) that HTML nodes inside contenteditable
 *     give us for free.
 *   - WebKit (Tauri on macOS) handles most macOS keybindings natively inside
 *     contenteditable — Cmd+←, Option+←, selections, clipboard — so we only
 *     need to intercept the commands that mean something ArcTerm-specific
 *     (Enter, Tab, ↑/↓, Escape, Ctrl+C).
 *
 * Responsibilities this file owns:
 *   - Render the editor, keep focus, show a blinking caret when empty.
 *   - Emit `onSubmit(text)` when Enter is pressed.
 *   - Forward Ctrl+C to the PTY via a caller-supplied `onInterrupt`.
 *   - Emit a placeholder for ↑ (history — Phase 3) and Tab (autocomplete —
 *     also Phase 3) so they don't leak into the shell in the meantime.
 */

export interface InputEditorOptions {
    host: HTMLElement;
    /** Fired when Enter is pressed. Receives the full text (can be multi-line). */
    onSubmit: (command: string) => void;
    /** Fired on Ctrl+C. Caller decides whether to forward as SIGINT. */
    onInterrupt?: () => void;
    /** Fired on ↑ — reserved for Phase 3 history overlay. */
    onHistoryUp?: () => void;
    /** Fired on ↓ — reserved for Phase 3 history overlay. */
    onHistoryDown?: () => void;
    /** Fired on Tab — reserved for Phase 3 autocomplete. */
    onComplete?: () => void;
}

export class InputEditor {
    private readonly el: HTMLDivElement;
    private readonly opts: InputEditorOptions;

    constructor(opts: InputEditorOptions) {
        this.opts = opts;

        // Build the editor element. We create it ourselves instead of taking
        // it from the DOM so callers only have to hand us a host container.
        const el = document.createElement("div");
        el.className = "arcterm-input-editor";
        el.contentEditable = "true";
        // spellcheck off: squiggly red underlines on shell commands are
        // annoying and break the monospaced grid.
        el.spellcheck = false;
        // `autocorrect` is non-standard but WebKit honors it.
        el.setAttribute("autocorrect", "off");
        el.setAttribute("autocapitalize", "off");
        el.setAttribute("aria-label", "ArcTerm command input");
        // Screen-reader role: treat as a textbox so assistive tech doesn't
        // mistake it for generic static content.
        el.setAttribute("role", "textbox");
        el.setAttribute("aria-multiline", "true");
        this.el = el;

        opts.host.appendChild(el);

        this.attachEventHandlers();

        // Focus on mount so the user can start typing immediately.
        requestAnimationFrame(() => el.focus());
    }

    /** Current editor contents, newlines preserved. */
    getValue(): string {
        // innerText preserves line breaks from <div>/<br> the way a user
        // expects ("what I see on screen"), unlike textContent which would
        // concatenate everything into one line.
        return this.el.innerText.replace(/\u00a0/g, " ");
    }

    /** Replace editor contents. Cursor lands at the end. */
    setValue(text: string): void {
        this.el.textContent = text;
        this.moveCursorToEnd();
    }

    /** Empty the editor. */
    clear(): void {
        this.el.textContent = "";
    }

    /** Grab focus. Called when the app regains window focus. */
    focus(): void {
        this.el.focus();
    }

    // -- internals -----------------------------------------------------------

    private attachEventHandlers(): void {
        this.el.addEventListener("keydown", this.onKeyDown);
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
                return;
            }
            // Plain Enter = submit.
            ev.preventDefault();
            const text = this.getValue();
            // A totally-empty submit (just Enter on a blank line) still
            // emits so the caller can echo a fresh prompt like real zsh.
            this.opts.onSubmit(text);
            this.clear();
            return;
        }

        // --- Ctrl+C: forward to PTY as SIGINT ---
        if (ev.ctrlKey && !ev.metaKey && !ev.altKey && ev.key === "c") {
            ev.preventDefault();
            // If there's a selection, let the browser do the copy — but in
            // this editor the user rarely needs to copy what they typed.
            // Check for selection first; if empty, interrupt.
            const sel = window.getSelection();
            if (sel && sel.toString().length > 0) {
                // Let default copy proceed.
                return;
            }
            this.opts.onInterrupt?.();
            this.clear();
            return;
        }

        // --- Tab: placeholder for autocomplete (Phase 3) ---
        if (ev.key === "Tab") {
            ev.preventDefault();
            this.opts.onComplete?.();
            return;
        }

        // --- ↑/↓: placeholder for history overlay (Phase 3) ---
        // We swallow these so they don't move the caret into multi-line
        // editing mode unexpectedly. In Phase 3 this opens a history browser.
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
        // normal typing — falls through to WebKit's built-in handling, which
        // implements macOS conventions correctly inside contenteditable.
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
