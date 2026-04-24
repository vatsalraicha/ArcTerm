/**
 * ArcTerm internal slash-commands.
 *
 * Commands starting with `/arcterm-` are intercepted BEFORE the text is
 * sent to the PTY. They're handled entirely in the app, but render
 * like a real command (block header + output + block footer) so the
 * user's mental model — "every line is a command with output" — stays
 * intact.
 *
 * The prefix is deliberately namespaced (`/arcterm-...`) so we can never
 * shadow a real shell command or a user's filename. Anything else
 * starting with `/` falls through to the shell as normal.
 *
 * Commands shipped in Phase 5b:
 *   /arcterm-help                 — list all internal commands
 *   /arcterm-model [mode]         — show / set AI backend mode
 *   /arcterm-models               — list downloadable models
 *   /arcterm-download <id>        — fetch a model from HuggingFace
 *   /arcterm-status               — show AI router state
 *
 * Output rendering: we write directly into the active session's xterm
 * buffer using `terminal.write(...)` with ANSI codes for color. The
 * block separator work the editor normally does on submit is mimicked
 * here via writeBlockStart/End so the command looks native.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { Session } from "./session-manager";

export const INTERNAL_PREFIX = "/arcterm-";

/**
 * Global hook registered by main.ts at boot. The theme command needs to
 * reach into SessionManager + flip a class on <html>, both of which live
 * outside this module. Rather than threading those through every
 * slash-command call site, main.ts stashes a handler here once.
 */
let themeApplier: ((theme: "dark" | "light") => void) | null = null;
export function registerThemeApplier(fn: (theme: "dark" | "light") => void): void {
    themeApplier = fn;
}

/**
 * True if this text should be handled internally rather than sent to the
 * shell. Editor's onSubmit checks this before the PTY send path.
 */
export function isInternalCommand(text: string): boolean {
    return text.trimStart().startsWith(INTERNAL_PREFIX);
}

interface ModelInfo {
    id: string;
    display_name: string;
    url: string;
    filename: string;
    size_bytes: number;
    parameters: string;
    quantization: string;
    license: string;
    installed: boolean;
}

interface AiStatus {
    mode: string;
    active_id: string;
    active_display_name: string;
    local_available: boolean;
    local_model: {
        id: string;
        display_name: string;
        quantization: string | null;
        parameters: string | null;
    } | null;
    // Wave 2.5: non-null while the background GGUF verify+mmap task is
    // in flight. Drives the "loading…" line in /arcterm-status and the
    // toolbar pill.
    local_loading?: {
        id: string;
        display_name: string;
        quantization?: string | null;
    } | null;
}

interface ProgressPayload {
    id: string;
    bytes_downloaded: number;
    bytes_total: number;
}

interface DonePayload {
    id: string;
    success: boolean;
    error?: string | null;
    local_path?: string | null;
}

/**
 * Execute an internal command against a specific session. The session
 * provides the xterm handle we write output to. Errors surface as red
 * lines; successes use the normal muted style.
 */
export async function runInternalCommand(
    raw: string,
    session: Session,
): Promise<void> {
    const trimmed = raw.trim();
    // Header pill (cwd + branch). For internal commands there's no
    // shell + no zle echo, so we ALSO write the command text manually
    // — otherwise the user would see "this output came from… what?".
    // For shell commands main.ts skips this manual write because zle
    // echoes the command for free.
    session.terminal.writeBlockStart(session.state.cwd, session.state.branch);
    session.terminal.writeRaw(`\x1b[1m${trimmed}\x1b[0m\r\n`);

    // Parse: `/arcterm-<name> [arg1] [arg2] ...`
    const parts = trimmed.slice(1).split(/\s+/);
    const name = parts[0]; // e.g. "arcterm-model"
    const args = parts.slice(1);

    let exitCode = 0;
    try {
        switch (name) {
            case "arcterm-help":
                writeHelp(session);
                break;
            case "arcterm-model":
                await cmdModel(session, args);
                break;
            case "arcterm-models":
                await cmdModels(session);
                break;
            case "arcterm-download":
                await cmdDownload(session, args);
                break;
            case "arcterm-status":
                await cmdStatus(session);
                break;
            case "arcterm-theme":
                await cmdTheme(session, args);
                break;
            case "arcterm-load":
                await cmdLoad(session, args);
                break;
            case "arcterm-audit":
                await cmdAudit(session, args);
                break;
            default:
                writeError(session, `Unknown command: ${name}`);
                writeLine(
                    session,
                    "Try `/arcterm-help` for the list of ArcTerm commands.",
                );
                exitCode = 127;
        }
    } catch (err) {
        writeError(session, errMessage(err));
        exitCode = 1;
    }

    session.terminal.writeBlockEnd(exitCode, null);
}

// -- Individual command impls -----------------------------------------

function writeHelp(session: Session): void {
    const rows: Array<[string, string]> = [
        ["/arcterm-help", "show this list"],
        ["/arcterm-model [claude|local|auto]", "show or set AI backend"],
        ["/arcterm-models", "list available local models"],
        ["/arcterm-download <id>", "download a model from the registry"],
        ["/arcterm-load <id>", "load a different installed model into memory"],
        ["/arcterm-theme [dark|light]", "show or set UI theme"],
        ["/arcterm-status", "show current AI router state"],
        ["/arcterm-audit [N]", "show recent IPC audit log entries"],
    ];
    writeLine(session, "\x1b[1mArcTerm commands:\x1b[0m");
    for (const [cmd, desc] of rows) {
        // Left column: 38 chars. Matches the widest command.
        const padded = cmd.padEnd(38, " ");
        writeLine(session, `  \x1b[36m${padded}\x1b[0m \x1b[2m${desc}\x1b[0m`);
    }
}

async function cmdModel(session: Session, args: string[]): Promise<void> {
    if (args.length === 0) {
        const status = await invoke<AiStatus>("ai_status");
        writeLine(
            session,
            `Current mode: \x1b[1;36m${status.mode}\x1b[0m (active: ${status.active_display_name})`,
        );
        writeLine(session, `Local model:  ${formatLocalModel(status)}`);
        writeLine(session, "Set with: /arcterm-model <claude|local|auto>");
        return;
    }
    const mode = args[0];
    if (!["claude", "local", "auto"].includes(mode)) {
        throw new Error(
            `Invalid mode '${mode}'. Use one of: claude, local, auto.`,
        );
    }
    await invoke("ai_set_mode", { mode });
    writeLine(
        session,
        `\x1b[32mSwitched AI mode to \x1b[1m${mode}\x1b[0m.`,
    );
}

async function cmdModels(session: Session): Promise<void> {
    const list = await invoke<ModelInfo[]>("model_list");
    if (list.length === 0) {
        writeLine(session, "No models registered.");
        return;
    }
    writeLine(session, "\x1b[1mAvailable models:\x1b[0m");
    for (const m of list) {
        const tag = m.installed
            ? "\x1b[32m● installed\x1b[0m"
            : "\x1b[2m○ not downloaded\x1b[0m";
        const size = formatSize(m.size_bytes);
        writeLine(
            session,
            `  \x1b[36m${m.id.padEnd(28, " ")}\x1b[0m ${m.display_name} — ${size} (${m.quantization}, ${m.license}) ${tag}`,
        );
    }
    writeLine(session, "\nDownload with: /arcterm-download <id>");
}

async function cmdDownload(session: Session, args: string[]): Promise<void> {
    if (args.length === 0) {
        throw new Error(
            "Specify a model id. Run /arcterm-models to see options.",
        );
    }
    // Shortcut: "/arcterm-download gemma" -> the default Gemma variant.
    // Saves users from having to type the exact id for the common case.
    let id = args[0];
    if (id === "gemma") id = "gemma-4-e2b-it-q4km";

    // SECURITY FIX (L-10): strip ANSI / control bytes out of the id
    // before we interpolate it into styled status lines. The Rust-side
    // allowlist will still reject an unknown id, but only AFTER we've
    // already written the "Starting download: <id>" echo. An id like
    // "\x1b[2J" would clear the screen before the rejection lands.
    // Users can't type \x1b via the editor, but programmatic sources
    // (history replay, URL handlers, pasted AI output) can.
    // eslint-disable-next-line no-control-regex
    const safeId = id.replace(/[\x00-\x1f\x7f]/g, "");

    writeLine(session, `Starting download: \x1b[36m${safeId}\x1b[0m`);
    writeLine(session, "This may take a few minutes depending on your connection.");
    writeLine(session, "");

    // Subscribe to progress + done events. Progress paints an in-place
    // line (\r carriage-return + erase + redraw); done unsubscribes.
    let unlistenProgress: UnlistenFn | null = null;
    let unlistenDone: UnlistenFn | null = null;
    let firstProgress = true;

    // SECURITY FIX: the original `new Promise(async (...) => {...})` would
    // swallow synchronous exceptions thrown while subscribing. Hoist the
    // subscription into a real async IIFE so errors propagate to the outer
    // try/catch below.
    const donePromise = new Promise<DonePayload>((resolve, reject) => {
        void (async () => {
            try {
                unlistenProgress = await listen<ProgressPayload>(
            "model://progress",
            (ev) => {
                if (ev.payload.id !== id) return;
                const pct = ev.payload.bytes_total > 0
                    ? Math.floor(
                        (ev.payload.bytes_downloaded / ev.payload.bytes_total) * 100,
                    )
                    : 0;
                const bar = renderProgressBar(pct);
                const current = formatSize(ev.payload.bytes_downloaded);
                const total = formatSize(ev.payload.bytes_total);
                // \r to return to start of line, \x1b[2K to erase, then redraw.
                // The first emit includes nothing special; subsequent emits
                // overwrite the previous line in place.
                if (!firstProgress) {
                    session.terminal.send; // noop — we write directly below.
                }
                firstProgress = false;
                // Write to the xterm buffer directly rather than through
                // terminal.send (which goes to the PTY). Direct write
                // bypasses the shell entirely.
                session.terminal.writeBlockStart; // typed handle doesn't expose raw write; use a helper
                writeRaw(
                    session,
                    `\r\x1b[2K  ${bar} ${pct}% \x1b[2m(${current} / ${total})\x1b[0m`,
                );
            },
        );
        unlistenDone = await listen<DonePayload>("model://done", (ev) => {
            if (ev.payload.id !== id) return;
            unlistenProgress?.();
            unlistenDone?.();
            resolve(ev.payload);
        });
            } catch (err) {
                reject(err);
            }
        })();
    });

    try {
        await invoke("model_download", { id });
    } catch (err) {
        unlistenProgress?.();
        unlistenDone?.();
        throw err;
    }

    const done = await donePromise;
    writeLine(session, ""); // newline after the progress bar
    if (done.success) {
        writeLine(
            session,
            `\x1b[32mDownload complete.\x1b[0m ${done.local_path ?? ""}`,
        );
        writeLine(
            session,
            "The model is loaded and AI mode has been switched to \x1b[1mauto\x1b[0m.",
        );
    } else {
        writeError(session, done.error ?? "download failed");
    }
}

async function cmdTheme(session: Session, args: string[]): Promise<void> {
    const settings = await invoke<{ theme?: string }>("settings_get");
    const current = settings.theme === "light" ? "light" : "dark";
    if (args.length === 0) {
        writeLine(
            session,
            `Current theme: \x1b[1;36m${current}\x1b[0m`,
        );
        writeLine(session, "Set with: /arcterm-theme <dark|light>");
        return;
    }
    const next = args[0];
    if (next !== "dark" && next !== "light") {
        throw new Error(`Invalid theme '${next}'. Use one of: dark, light.`);
    }
    if (!themeApplier) {
        throw new Error("theme applier not registered");
    }
    // Persist first, then apply. settings_set replaces the full Settings
    // object so we have to round-trip: fetch, modify, save.
    const full = await invoke<Record<string, unknown>>("settings_get");
    await invoke("settings_set", { settings: { ...full, theme: next } });
    themeApplier(next);
    writeLine(session, `\x1b[32mTheme switched to \x1b[1m${next}\x1b[0m.`);
}

/**
 * /arcterm-load <id> — swap the active local model without changing the
 * AI backend mode. Useful for A/B'ing quantizations (e.g. flip between
 * Q4_K_M and Q8_0 to compare answer quality) without touching Claude/auto
 * settings. Prints a tiny progress line while the new model's Metal
 * shaders compile (typically a few seconds the first time).
 */
async function cmdLoad(session: Session, args: string[]): Promise<void> {
    if (args.length === 0) {
        writeLine(session, "Usage: /arcterm-load <model-id>");
        writeLine(session, "Run /arcterm-models to see available ids.");
        return;
    }
    const id = args[0];
    writeLine(
        session,
        `Loading \x1b[36m${id}\x1b[0m… \x1b[2m(Metal shader compile is slow on first load)\x1b[0m`,
    );
    try {
        await invoke("ai_set_local_model", { id });
        const status = await invoke<AiStatus>("ai_status");
        writeLine(
            session,
            `\x1b[32mLoaded.\x1b[0m ${formatLocalModel(status)}`,
        );
    } catch (err) {
        throw err instanceof Error ? err : new Error(String(err));
    }
}

/**
 * /arcterm-audit [N] — show the last N entries of the IPC audit log.
 *
 * Wave 5 (lite) forensic surface. Defaults to the last 20 rows.
 * Flagged entries (OSC 52 in pty_write, destructive substrings, etc.)
 * render in red; normal entries in muted gray. The entire log is
 * ring-buffered in memory, so old entries disappear on boot and a
 * busy session caps out at 200 rows total.
 *
 * Not a security gate — purely for the user investigating "why did
 * ArcTerm do X?" after a weirdness report. See ipc_guard.rs docs
 * for the full rationale.
 */
interface AuditEntry {
    command: string;
    timestamp_ms: number;
    bytes: number;
    flag: string | null;
}
async function cmdAudit(session: Session, args: string[]): Promise<void> {
    const limit = args.length > 0 ? Math.max(1, Math.min(200, parseInt(args[0], 10) || 20)) : 20;
    const rows = await invoke<AuditEntry[]>("ipc_audit_tail", { limit });
    if (rows.length === 0) {
        writeLine(session, "\x1b[2m(audit log is empty)\x1b[0m");
        return;
    }
    writeLine(
        session,
        `\x1b[1mRecent IPC calls\x1b[0m \x1b[2m(${rows.length} of up to 200 in ring buffer)\x1b[0m`,
    );
    // Newest last — matches the natural reading order of a log tail.
    for (const row of rows) {
        const when = new Date(row.timestamp_ms).toLocaleTimeString();
        const cmd = row.command.padEnd(18);
        const size = `${row.bytes}B`.padStart(8);
        if (row.flag) {
            writeLine(
                session,
                `  \x1b[2m${when}\x1b[0m \x1b[36m${cmd}\x1b[0m ${size}  \x1b[31m⚠ ${row.flag}\x1b[0m`,
            );
        } else {
            writeLine(
                session,
                `  \x1b[2m${when}\x1b[0m \x1b[36m${cmd}\x1b[0m ${size}`,
            );
        }
    }
    writeLine(session, "");
    writeLine(
        session,
        "\x1b[2mRed ⚠ rows flagged by the content sniffer — forensic only, never blocks.\x1b[0m",
    );
}

async function cmdStatus(session: Session): Promise<void> {
    const status = await invoke<AiStatus>("ai_status");
    writeLine(session, `Mode:         \x1b[1m${status.mode}\x1b[0m`);
    writeLine(
        session,
        `Active:       ${status.active_display_name} (${status.active_id})`,
    );
    writeLine(session, `Local model:  ${formatLocalModel(status)}`);
}

/**
 * Format the local-model line for both /arcterm-status and /arcterm-model.
 * Returns either "ready — Gemma 4 E2B (Q4_K_M) [2.3B active / 5.1B total]"
 * when a variant is loaded, or a colored "ready" / "not loaded" fallback.
 */
function formatLocalModel(status: AiStatus): string {
    // Background load in flight (Wave 2.5 boot path). Surfaces the
    // pending variant name so the user knows which file is being
    // hashed+mmap'd, matching the toolbar pill's text.
    if (status.local_loading) {
        const q = status.local_loading.quantization
            ? ` (${status.local_loading.quantization})`
            : "";
        return `\x1b[36mloading…\x1b[0m ${status.local_loading.display_name}${q}`;
    }
    if (!status.local_available) {
        return "\x1b[33mnot loaded\x1b[0m";
    }
    const m = status.local_model;
    if (!m) {
        return "\x1b[32mready\x1b[0m";
    }
    const meta = [m.quantization, m.parameters]
        .filter((s): s is string => Boolean(s))
        .join(", ");
    const metaSuffix = meta ? ` \x1b[2m(${meta})\x1b[0m` : "";
    return `\x1b[32mready\x1b[0m — ${m.display_name}${metaSuffix}`;
}

// -- Rendering helpers -------------------------------------------------

function writeLine(session: Session, line: string): void {
    writeRaw(session, line + "\r\n");
}

function writeError(session: Session, msg: string): void {
    writeLine(session, `\x1b[31mError:\x1b[0m ${msg}`);
}

/**
 * Write raw bytes (including ANSI escapes + newlines) into the active
 * session's xterm buffer. TerminalHandle doesn't expose a direct write
 * method today, so we piggyback via `writeBlockEnd` -> "\n" which we
 * already know is safe. For now we just stuff into the same ANSI stream
 * by calling the session's raw terminal behind the handle. Since the
 * handle's API surface is limited, we temporarily re-use the send path:
 * no — send goes to PTY. We need a real raw-write exit.
 *
 * Solution: we added no new handle method; the session owns its xterm
 * internally and only exposes send/write*. Workaround: use the
 * `writeBlockStart` + `writeBlockEnd` pair to frame the output block,
 * and drop the internal bytes inline via write_terminal_raw on the
 * handle. For this to work we add a `writeRaw` method to TerminalHandle.
 */
function writeRaw(session: Session, bytes: string): void {
    session.terminal.writeRaw(bytes);
}

function renderProgressBar(pct: number): string {
    const width = 24;
    const filled = Math.round((pct / 100) * width);
    const empty = width - filled;
    return (
        "\x1b[38;2;79;195;247m" +
        "█".repeat(filled) +
        "\x1b[2m" +
        "░".repeat(empty) +
        "\x1b[0m"
    );
}

function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    const units = ["KB", "MB", "GB", "TB"];
    let i = -1;
    let b = bytes;
    do {
        b /= 1024;
        i++;
    } while (b >= 1024 && i < units.length - 1);
    return `${b.toFixed(b < 10 ? 1 : 0)} ${units[i]}`;
}

function errMessage(e: unknown): string {
    if (e instanceof Error) return e.message;
    if (typeof e === "string") return e;
    return JSON.stringify(e);
}
