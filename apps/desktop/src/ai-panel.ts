/**
 * AI panel — Cmd+K command generation + Cmd+Shift+E explain.
 *
 * One component, two display modes. The panel lives as a floating card
 * anchored to the input dock. Opening it focuses the textarea; Escape
 * dismisses.
 *
 * Command mode (Cmd+K):
 *   - Prompt: "describe what you want to do"
 *   - On submit: calls ai_ask(mode="command") — non-streaming for a tight
 *     feel (the answer is usually one line and short).
 *   - Result shows a proposed command with Run / Edit / Dismiss buttons.
 *     Run sends to the active session; Edit populates the input editor.
 *
 * Explain mode (Cmd+Shift+E):
 *   - Prompt pre-filled with the target (last-error context or current
 *     editor contents).
 *   - Streams the response into the panel body as it arrives.
 *   - If the model's reply ends in a ```shell fence with a single command,
 *     a "Run fix" button appears.
 *
 * Neither mode mounts persistent state — close the panel and it resets.
 */

import {
    aiAsk,
    aiStream,
    aiActiveBackend,
    classifyRisk,
    extractCommand,
    hasDangerousInvisibles,
    type AiContext,
} from "./ai";

export interface AiPanelOptions {
    /** Container where the panel is appended (usually #app). */
    host: HTMLElement;
    /** Send a command to the active session. Called from the "Run" button. */
    runCommand: (cmd: string) => void;
    /** Populate the input editor (for "Edit" button or when the user wants
     *  to tweak the AI's suggestion before running). */
    populateEditor: (text: string) => void;
    /** Return the currently relevant AiContext — active session's cwd,
     *  branch, recent commands, last-error if applicable. Called at the
     *  moment a prompt is submitted. */
    getContext: () => AiContext;
    /** Return the currently selected explain target. For Cmd+Shift+E. */
    getExplainTarget: () => ExplainTarget | null;
    /** Refocus the input editor when the panel closes. */
    focusEditor: () => void;
}

export type ExplainTarget =
    | { kind: "error"; command: string; output: string; exitCode: number }
    | { kind: "command"; command: string };

export class AiPanel {
    private readonly opts: AiPanelOptions;
    private readonly root: HTMLDivElement;
    private readonly title: HTMLElement;
    private readonly textarea: HTMLTextAreaElement;
    private readonly submitBtn: HTMLButtonElement;
    private readonly status: HTMLElement;
    private readonly output: HTMLElement;
    private readonly actions: HTMLElement;
    private inFlight: { cancel: () => void } | null = null;
    private open_ = false;
    private mode: "command" | "explain" = "command";
    private suggestedCommand: string | null = null;

    constructor(opts: AiPanelOptions) {
        this.opts = opts;

        const root = document.createElement("div");
        root.className = "arcterm-ai-panel hidden";
        root.setAttribute("role", "dialog");
        root.setAttribute("aria-label", "ArcTerm AI");
        root.tabIndex = -1;

        const panel = document.createElement("div");
        panel.className = "arcterm-ai-card";

        const header = document.createElement("div");
        header.className = "arcterm-ai-header";
        const title = document.createElement("span");
        title.className = "arcterm-ai-title";
        title.textContent = "Ask Claude";
        const close = document.createElement("button");
        close.type = "button";
        close.className = "arcterm-ai-close";
        close.textContent = "×";
        close.setAttribute("aria-label", "Close");
        close.addEventListener("click", () => this.close());
        header.append(title, close);

        const form = document.createElement("div");
        form.className = "arcterm-ai-form";
        const textarea = document.createElement("textarea");
        textarea.className = "arcterm-ai-input";
        textarea.placeholder = "Describe what you want to do…";
        textarea.rows = 3;
        textarea.spellcheck = false;
        textarea.setAttribute("autocorrect", "off");
        textarea.setAttribute("autocapitalize", "off");
        const submitRow = document.createElement("div");
        submitRow.className = "arcterm-ai-submit-row";
        const hint = document.createElement("span");
        hint.className = "arcterm-ai-hint";
        hint.textContent = "⏎ to send · Esc to dismiss";
        const submit = document.createElement("button");
        submit.type = "button";
        submit.className = "arcterm-ai-submit";
        submit.textContent = "Send";
        submitRow.append(hint, submit);
        form.append(textarea, submitRow);

        const status = document.createElement("div");
        status.className = "arcterm-ai-status";

        const output = document.createElement("div");
        output.className = "arcterm-ai-output";

        const actions = document.createElement("div");
        actions.className = "arcterm-ai-actions hidden";

        panel.append(header, form, status, output, actions);
        root.append(panel);
        opts.host.append(root);

        this.root = root;
        this.title = title;
        this.textarea = textarea;
        this.submitBtn = submit;
        this.status = status;
        this.output = output;
        this.actions = actions;

        submit.addEventListener("click", () => this.run());
        textarea.addEventListener("keydown", (ev) => this.onTextKey(ev));
        root.addEventListener("keydown", (ev) => {
            if (ev.key === "Escape") {
                ev.preventDefault();
                this.close();
            }
        });
        root.addEventListener("mousedown", (ev) => {
            if (ev.target === root) this.close();
        });
    }

    isOpen(): boolean {
        return this.open_;
    }

    async openForCommand(): Promise<void> {
        this.mode = "command";
        this.title.textContent = "Generate command";
        this.textarea.placeholder =
            "Describe what you want to do (e.g. find all .py files modified today)";
        this.textarea.value = "";
        this.resetOutput();
        this.show();
        // Label which backend will answer.
        const active = await aiActiveBackend();
        this.status.textContent = active
            ? `Will answer via ${active.display_name}`
            : "";
        this.textarea.focus();
    }

    async openForExplain(): Promise<void> {
        this.mode = "explain";
        this.title.textContent = "Explain";
        this.resetOutput();
        const target = this.opts.getExplainTarget();
        if (!target) {
            this.textarea.placeholder =
                "Paste a command or error to explain";
            this.textarea.value = "";
            this.show();
            this.textarea.focus();
            return;
        }
        this.textarea.placeholder = "Anything to add? (optional)";
        this.textarea.value = "";
        this.show();

        const active = await aiActiveBackend();
        this.status.textContent = target.kind === "error"
            ? `Explaining last error${active ? ` via ${active.display_name}` : ""}`
            : `Explaining command${active ? ` via ${active.display_name}` : ""}`;
        // Kick off the stream immediately — user didn't type anything yet,
        // but they summoned the panel specifically to get an explanation.
        this.runExplain(target, "");
        this.textarea.focus();
    }

    close(): void {
        if (!this.open_) return;
        this.open_ = false;
        this.root.classList.add("hidden");
        if (this.inFlight) {
            this.inFlight.cancel();
            this.inFlight = null;
        }
        this.opts.focusEditor();
    }

    // -- internals ---------------------------------------------------------

    private show(): void {
        this.open_ = true;
        this.root.classList.remove("hidden");
    }

    private resetOutput(): void {
        this.output.textContent = "";
        this.output.classList.remove("has-content");
        this.actions.innerHTML = "";
        this.actions.classList.add("hidden");
        this.status.textContent = "";
        this.suggestedCommand = null;
        this.submitBtn.disabled = false;
        this.submitBtn.textContent = "Send";
    }

    private onTextKey(ev: KeyboardEvent): void {
        if (ev.key === "Enter" && !ev.shiftKey) {
            ev.preventDefault();
            this.run();
        }
    }

    private async run(): Promise<void> {
        const prompt = this.textarea.value.trim();
        if (!prompt && this.mode === "command") return;

        if (this.mode === "command") {
            await this.runCommandGen(prompt);
        } else {
            const target = this.opts.getExplainTarget();
            await this.runExplain(target, prompt);
        }
    }

    private async runCommandGen(prompt: string): Promise<void> {
        this.submitBtn.disabled = true;
        this.submitBtn.textContent = "Thinking…";
        this.resetOutput();
        this.submitBtn.disabled = true;
        this.submitBtn.textContent = "Thinking…";
        try {
            const res = await aiAsk({
                prompt,
                mode: "command",
                context: this.opts.getContext(),
            });
            const cmd = extractCommand(res.text);
            // `null` = extractor rejected the output. Two reasons today:
            // the model returned empty/whitespace, or the candidate
            // command contained Trojan-Source-class invisible chars
            // (bidi overrides, zero-width splitters). Either way we
            // refuse to surface a Run button.
            if (!cmd) {
                this.suggestedCommand = null;
                this.output.textContent = hasDangerousInvisibles(res.text)
                    ? "Rejected: the model's output contained invisible Unicode characters (possible prompt-injection attack). Try rephrasing."
                    : "(no command returned)";
                this.output.classList.add("has-content");
                return;
            }
            this.suggestedCommand = cmd;
            this.output.textContent = cmd;
            this.output.classList.add("has-content");
            this.renderCommandActions(cmd);
        } catch (err) {
            this.output.textContent = `Error: ${formatError(err)}`;
            this.output.classList.add("has-content");
        } finally {
            this.submitBtn.disabled = false;
            this.submitBtn.textContent = "Send";
        }
    }

    private async runExplain(
        target: ExplainTarget | null,
        extra: string,
    ): Promise<void> {
        if (this.inFlight) {
            this.inFlight.cancel();
            this.inFlight = null;
        }
        this.submitBtn.disabled = true;
        this.submitBtn.textContent = "Thinking…";
        this.output.textContent = "";
        this.output.classList.add("has-content");
        this.actions.innerHTML = "";
        this.actions.classList.add("hidden");

        const prompt = buildExplainPrompt(target, extra);
        const context = this.opts.getContext();
        if (target && target.kind === "error") {
            context.failing_command = target.command;
            context.failing_output = target.output;
            context.failing_exit_code = target.exitCode;
        }

        const handle = aiStream(
            { prompt, mode: "explain", context },
            (delta) => {
                this.output.textContent += delta;
                // Scroll to follow the stream.
                this.output.scrollTop = this.output.scrollHeight;
            },
        );
        this.inFlight = handle;

        try {
            const full = await handle.promise;
            // Look for a ```sh|bash|zsh fenced one-liner at the end; that's
            // the suggested fix command. If found, surface a Run button.
            const fix = extractFixCommand(full);
            if (fix) {
                this.suggestedCommand = fix;
                this.renderCommandActions(fix);
            }
        } catch (err) {
            this.output.textContent += `\n\nError: ${formatError(err)}`;
        } finally {
            this.inFlight = null;
            this.submitBtn.disabled = false;
            this.submitBtn.textContent = "Send";
        }
    }

    private renderCommandActions(cmd: string): void {
        this.actions.innerHTML = "";
        this.actions.classList.remove("hidden");

        // SECURITY (Wave 3): AI-suggested commands that match a
        // destructive heuristic go through a two-stage confirmation.
        // First click: Run button swaps to a red "Confirm run" with
        // the reason displayed. Second click actually submits. Edit
        // and Copy stay available on both stages — users can inspect
        // or sanitize the proposed command before committing.
        const risk = classifyRisk(cmd);
        let confirmed = false;

        const renderButtons = () => {
            this.actions.innerHTML = "";
            if (risk) {
                const warn = document.createElement("div");
                warn.className = "arcterm-ai-risk";
                warn.textContent = confirmed
                    ? `Click "Confirm run" to execute — this matches: ${risk}`
                    : `⚠ Destructive pattern detected: ${risk}. Review carefully before running.`;
                this.actions.append(warn);
            }
            const runLabel = risk
                ? (confirmed ? "Confirm run" : "Run…")
                : "Run";
            const runVariant: "primary" | "danger" = risk ? "danger" : "primary";
            const run = button(runLabel, runVariant, () => {
                if (risk && !confirmed) {
                    confirmed = true;
                    renderButtons();
                    return;
                }
                this.opts.runCommand(cmd);
                this.close();
            });
            const edit = button("Edit", "ghost", () => {
                this.opts.populateEditor(cmd);
                this.close();
            });
            const copy = button("Copy", "ghost", () => {
                navigator.clipboard.writeText(cmd).catch(() => {});
            });
            this.actions.append(run, edit, copy);
        };
        renderButtons();
    }
}

function button(
    label: string,
    variant: "primary" | "ghost" | "danger",
    onClick: () => void,
): HTMLButtonElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `arcterm-ai-btn ${variant}`;
    b.textContent = label;
    b.addEventListener("click", onClick);
    return b;
}

function buildExplainPrompt(
    target: ExplainTarget | null,
    extra: string,
): string {
    if (!target) {
        return extra || "Explain the context.";
    }
    if (target.kind === "error") {
        const base = `The command \`${target.command}\` exited with code ${target.exitCode}.`;
        return extra ? `${base}\n\n${extra}` : base;
    }
    const base = `Explain what this command does, step by step: ${target.command}`;
    return extra ? `${base}\n\n${extra}` : base;
}

/**
 * Grab the last fenced one-liner from the explain output so we can surface
 * a "Run fix" button. Returns null if no runnable suggestion is present,
 * or if the candidate contains Trojan-Source-class invisible characters
 * (same security gate as `extractCommand`).
 */
function extractFixCommand(text: string): string | null {
    const m = text.match(/```(?:sh|bash|zsh)?\s*\n([^\n]+?)\n```\s*$/);
    if (!m) return null;
    const line = m[1].trim();
    if (!line) return null;
    if (hasDangerousInvisibles(line)) return null;
    return line;
}

function formatError(err: unknown): string {
    if (err instanceof Error) return err.message;
    if (typeof err === "string") return err;
    return JSON.stringify(err);
}
