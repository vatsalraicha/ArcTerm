/**
 * Settings panel — a modal card triggered by ⌘, (standard macOS
 * convention). Lets the user switch theme, AI backend mode, active
 * local model, and Claude CLI path from one place instead of spelunking
 * through `/arcterm-*` slash commands.
 *
 * All changes are LIVE. Hitting "Save" persists to ~/.arcterm/config.json
 * and immediately applies:
 *   - Theme flip via the shared themeApplier hook.
 *   - AI mode change via ai_set_mode (router swap; also persists).
 *   - Local model change via /arcterm-model-load-style path (rare —
 *     covered for completeness).
 *   - Claude path is persisted only; the next Claude request picks it up.
 *
 * Form values are read from `settings_get` each time the panel opens,
 * so changes made via slash commands since the last open are reflected.
 * Close without Save discards unsaved edits.
 */

import { invoke } from "@tauri-apps/api/core";

import type { ThemeName } from "./terminal";

interface Settings {
    theme?: string;
    ai?: {
        mode?: string;
        localModel?: string;
        claudePath?: string;
    };
}

interface ModelInfo {
    id: string;
    display_name: string;
    installed: boolean;
    quantization?: string;
    parameters?: string;
}

export interface SettingsPanelOptions {
    /** Parent element to mount the overlay into (usually #app). */
    host: HTMLElement;
    /** Apply a theme live (identical signature to themeApplier). */
    applyTheme: (theme: ThemeName) => void;
    /** Refocus the editor when the panel closes. */
    focusEditor: () => void;
}

export class SettingsPanel {
    private readonly opts: SettingsPanelOptions;
    private readonly root: HTMLDivElement;
    private readonly form: HTMLFormElement;
    private open_ = false;
    private statusEl!: HTMLDivElement;

    constructor(opts: SettingsPanelOptions) {
        this.opts = opts;

        const root = document.createElement("div");
        root.className = "arcterm-settings-overlay hidden";
        root.setAttribute("role", "dialog");
        root.setAttribute("aria-label", "ArcTerm settings");
        root.tabIndex = -1;

        const panel = document.createElement("div");
        panel.className = "arcterm-settings-card";

        const header = document.createElement("div");
        header.className = "arcterm-settings-header";
        const title = document.createElement("span");
        title.className = "arcterm-settings-title";
        title.textContent = "Settings";
        const close = document.createElement("button");
        close.type = "button";
        close.className = "arcterm-settings-close";
        close.textContent = "×";
        close.setAttribute("aria-label", "Close");
        close.addEventListener("click", () => this.close());
        header.append(title, close);

        const form = document.createElement("form");
        form.className = "arcterm-settings-form";
        form.addEventListener("submit", (e) => {
            e.preventDefault();
            void this.save();
        });
        this.form = form;

        const status = document.createElement("div");
        status.className = "arcterm-settings-status";
        this.statusEl = status;

        panel.append(header, form, status);
        root.append(panel);
        opts.host.append(root);

        root.addEventListener("keydown", (ev) => {
            if (ev.key === "Escape") {
                ev.preventDefault();
                this.close();
            }
        });
        root.addEventListener("mousedown", (ev) => {
            if (ev.target === root) this.close();
        });

        this.root = root;
    }

    isOpen(): boolean {
        return this.open_;
    }

    async open(): Promise<void> {
        if (this.open_) return;
        this.open_ = true;
        this.root.classList.remove("hidden");
        await this.render();
        this.root.focus();
    }

    close(): void {
        if (!this.open_) return;
        this.open_ = false;
        this.root.classList.add("hidden");
        this.opts.focusEditor();
    }

    // -- internals ---------------------------------------------------------

    private async render(): Promise<void> {
        let settings: Settings;
        let models: ModelInfo[];
        try {
            [settings, models] = await Promise.all([
                invoke<Settings>("settings_get"),
                invoke<ModelInfo[]>("model_list"),
            ]);
        } catch (err) {
            this.form.innerHTML = "";
            const msg = document.createElement("div");
            msg.className = "arcterm-settings-error";
            msg.textContent = `Failed to load settings: ${String(err)}`;
            this.form.append(msg);
            return;
        }

        this.form.innerHTML = "";

        // Theme — simple dark/light radio.
        this.form.append(
            section(
                "Appearance",
                radioGroup("theme", settings.theme ?? "dark", [
                    { value: "dark", label: "Dark" },
                    { value: "light", label: "Light" },
                ]),
            ),
        );

        // AI backend mode — three-way radio.
        const aiMode = settings.ai?.mode ?? "auto";
        this.form.append(
            section(
                "AI backend",
                radioGroup("aiMode", aiMode, [
                    { value: "claude", label: "Claude CLI only" },
                    { value: "local", label: "Local Gemma only" },
                    { value: "auto", label: "Auto — Claude first, fall back to local" },
                ]),
            ),
        );

        // Local model — dropdown of INSTALLED models. If none, disable.
        const installed = models.filter((m) => m.installed);
        const localModelEl = document.createElement("select");
        localModelEl.name = "localModel";
        if (installed.length === 0) {
            const opt = document.createElement("option");
            opt.value = "";
            opt.textContent = "(no local models installed)";
            localModelEl.disabled = true;
            localModelEl.append(opt);
        } else {
            for (const m of installed) {
                const opt = document.createElement("option");
                opt.value = m.id;
                opt.textContent = m.display_name;
                if (m.id === settings.ai?.localModel) opt.selected = true;
                localModelEl.append(opt);
            }
        }
        this.form.append(
            section(
                "Local model",
                wrap(localModelEl, installed.length === 0
                    ? "Use /arcterm-download to install a model."
                    : "Which model the local backend should load at startup."),
            ),
        );

        // Claude path — text input.
        const claudePathEl = document.createElement("input");
        claudePathEl.type = "text";
        claudePathEl.name = "claudePath";
        claudePathEl.placeholder = "claude (PATH lookup)";
        claudePathEl.value = settings.ai?.claudePath ?? "";
        this.form.append(
            section(
                "Claude CLI path",
                wrap(claudePathEl,
                    "Leave empty to use whatever `claude` resolves to via PATH. Set an absolute path if you have multiple installs."),
            ),
        );

        // Actions: Save / Cancel.
        const actions = document.createElement("div");
        actions.className = "arcterm-settings-actions";
        const cancel = document.createElement("button");
        cancel.type = "button";
        cancel.className = "arcterm-settings-btn ghost";
        cancel.textContent = "Cancel";
        cancel.addEventListener("click", () => this.close());
        const save = document.createElement("button");
        save.type = "submit";
        save.className = "arcterm-settings-btn primary";
        save.textContent = "Save";
        actions.append(cancel, save);
        this.form.append(actions);

        this.statusEl.textContent = "";
    }

    private async save(): Promise<void> {
        const fd = new FormData(this.form);
        const theme = (fd.get("theme") as string | null) ?? "dark";
        const aiMode = (fd.get("aiMode") as string | null) ?? "auto";
        const localModel = (fd.get("localModel") as string | null) ?? "";
        const claudePath = (fd.get("claudePath") as string | null) ?? "";

        this.statusEl.textContent = "Saving…";
        try {
            // Persist full settings in one call — the Rust side expects
            // the whole Settings object, not a partial.
            const current = await invoke<Settings>("settings_get");
            const next: Settings = {
                ...current,
                theme,
                ai: {
                    ...(current.ai ?? {}),
                    mode: aiMode,
                    localModel,
                    claudePath,
                },
            };
            await invoke("settings_set", { settings: next });

            // Apply live changes. Order matters:
            //   1. Swap the local model FIRST if the user picked a new
            //      one; that writes into the router synchronously (once
            //      the Metal shaders finish) so step 2 can observe it.
            //   2. Set the mode, which may use the freshly-loaded model.
            // Persisting the mode via step 2 also re-saves localModel as
            // a side-effect (ai_set_local_model already did), but
            // idempotent writes are fine.
            let loadWarning: string | null = null;
            if (localModel) {
                try {
                    await invoke("ai_set_local_model", { id: localModel });
                } catch (err) {
                    loadWarning = `Model load: ${String(err)}`;
                }
            }
            try {
                await invoke("ai_set_mode", { mode: aiMode });
            } catch (err) {
                // Mode mismatch (e.g. local requested but no model) —
                // show but keep other saves.
                this.statusEl.textContent = `Saved. AI mode warning: ${String(err)}`;
            }
            if (loadWarning) {
                this.statusEl.textContent = `Saved. ${loadWarning}`;
            }
            this.opts.applyTheme(theme === "light" ? "light" : "dark");
            if (!this.statusEl.textContent) {
                this.statusEl.textContent = "Saved.";
            }
            // Close after a brief "Saved." flash so the user knows it landed.
            setTimeout(() => this.close(), 450);
        } catch (err) {
            this.statusEl.textContent = `Save failed: ${String(err)}`;
        }
    }
}

// --- small DOM helpers. Keep the panel file self-contained. --------------

function section(titleText: string, body: HTMLElement): HTMLDivElement {
    const el = document.createElement("div");
    el.className = "arcterm-settings-section";
    const t = document.createElement("div");
    t.className = "arcterm-settings-section-title";
    t.textContent = titleText;
    el.append(t, body);
    return el;
}

function radioGroup(
    name: string,
    selected: string,
    options: Array<{ value: string; label: string }>,
): HTMLDivElement {
    const el = document.createElement("div");
    el.className = "arcterm-settings-radio-group";
    for (const opt of options) {
        const label = document.createElement("label");
        label.className = "arcterm-settings-radio";
        const input = document.createElement("input");
        input.type = "radio";
        input.name = name;
        input.value = opt.value;
        if (opt.value === selected) input.checked = true;
        const span = document.createElement("span");
        span.textContent = opt.label;
        label.append(input, span);
        el.append(label);
    }
    return el;
}

function wrap(control: HTMLElement, hint: string): HTMLDivElement {
    const el = document.createElement("div");
    el.className = "arcterm-settings-control";
    const hintEl = document.createElement("div");
    hintEl.className = "arcterm-settings-hint";
    hintEl.textContent = hint;
    el.append(control, hintEl);
    return el;
}
