/**
 * AI IPC wrappers + stream consumption.
 *
 * The Rust backend exposes four commands:
 *   ai_is_available()                    -> boolean
 *   ai_active_backend()                  -> { id, display_name }
 *   ai_ask({ prompt, context?, mode })   -> { text, backend }
 *   ai_stream({ prompt, context?, mode}) -> id (streams via event)
 *
 * Streaming arrives on the `ai://chunk` event:
 *   { id, delta, done, error? }
 * We correlate chunks by id so multiple concurrent streams don't collide.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type AiMode = "chat" | "command" | "explain";

export interface AiContext {
    cwd?: string | null;
    shell?: string | null;
    git_branch?: string | null;
    recent_commands?: string[];
    failing_command?: string | null;
    failing_output?: string | null;
    failing_exit_code?: number | null;
}

export interface AiRequest {
    prompt: string;
    mode?: AiMode;
    context?: AiContext;
}

export interface AiResponse {
    text: string;
    backend: string;
}

export interface AiBackendInfo {
    id: string;
    display_name: string;
}

interface AiChunk {
    id: string;
    delta: string;
    done: boolean;
    error?: string | null;
}

let availabilityCache: boolean | null = null;
let activeBackendCache: AiBackendInfo | null = null;

export async function aiIsAvailable(force = false): Promise<boolean> {
    if (!force && availabilityCache !== null) return availabilityCache;
    try {
        availabilityCache = await invoke<boolean>("ai_is_available");
    } catch (err) {
        console.error("ai_is_available failed", err);
        availabilityCache = false;
    }
    return availabilityCache;
}

export async function aiActiveBackend(): Promise<AiBackendInfo | null> {
    if (activeBackendCache) return activeBackendCache;
    try {
        activeBackendCache = await invoke<AiBackendInfo>("ai_active_backend");
    } catch (err) {
        console.error("ai_active_backend failed", err);
        activeBackendCache = null;
    }
    return activeBackendCache;
}

export async function aiAsk(req: AiRequest): Promise<AiResponse> {
    return await invoke<AiResponse>("ai_ask", { req });
}

/**
 * Start a streaming request. Returns a handle with:
 *   - `promise`: resolves with the full concatenated text when the stream
 *     finishes successfully, or rejects with the first error.
 *   - `cancel()`: drop the event listener. The Rust side will continue
 *     running until the subprocess ends — see TODO below.
 *
 * `onDelta` fires for every non-empty chunk, so UIs can render partial
 * text as it arrives without waiting for promise resolution.
 *
 * TODO Phase 5b: add an `ai_cancel(id)` command that kills the subprocess
 * on the Rust side. Today cancel() just stops the listener; the CLI process
 * runs to completion but its output is discarded.
 */
export function aiStream(
    req: AiRequest,
    onDelta: (delta: string) => void,
): {
    promise: Promise<string>;
    cancel: () => void;
} {
    let buffer = "";
    let unlisten: UnlistenFn | null = null;
    let settled = false;

    // SECURITY FIX: the original `new Promise(async (...) => {...})`
    // pattern swallows synchronous exceptions thrown inside the executor.
    // Factor the async work into its own function and explicitly resolve /
    // reject so every failure path surfaces to the caller.
    const promise = (async (): Promise<string> => {
        const requestId = await invoke<string>("ai_stream", { req });
        return await new Promise<string>((resolve, reject) => {
            listen<AiChunk>("ai://chunk", (ev) => {
                if (ev.payload.id !== requestId) return;
                if (ev.payload.delta) {
                    buffer += ev.payload.delta;
                    onDelta(ev.payload.delta);
                }
                if (ev.payload.done) {
                    if (unlisten) unlisten();
                    settled = true;
                    if (ev.payload.error) {
                        reject(new Error(ev.payload.error));
                    } else {
                        resolve(buffer);
                    }
                }
            }).then((fn) => {
                unlisten = fn;
            }).catch((err) => {
                settled = true;
                reject(err);
            });
        });
    })();

    return {
        promise,
        cancel: () => {
            if (!settled && unlisten) {
                unlisten();
                unlisten = null;
                settled = true;
            }
        },
    };
}

/**
 * SECURITY: Unicode codepoints whose presence in an AI-generated command
 * indicates a Trojan-Source-style visual/byte mismatch attack
 * (CVE-2021-42574 class).
 *
 *   - Bidi controls (U+202A-E, U+2066-9): reverse the visual reading
 *     order so the displayed text looks benign while the bytes sent
 *     to the shell are malicious.
 *   - Zero-width characters (U+200B-D, U+FEFF, U+2060): invisible
 *     separators that can split or embed payload text inside what
 *     looks like an ordinary identifier.
 *   - Line/paragraph separators (U+2028-9): can terminate a shell
 *     line in ways the user won't see when the model's output is
 *     rendered as plain text.
 *   - Other known-invisible codepoints (soft hyphen U+00AD,
 *     combining-grapheme-joiner U+034F, various Hangul fillers and
 *     Mongolian vowel separator).
 *
 * On detection we reject the command outright — `extractCommand` and
 * `extractFixCommand` return `null`. Callers surface a user-visible
 * error instead of proposing the command; never silently strip, since
 * attackers could craft the remainder to still be destructive even
 * after the invisibles are removed.
 */
const DANGEROUS_INVISIBLE_CHARS =
    /[\u00AD\u034F\u115F\u1160\u17B4\u17B5\u180E\u200B-\u200F\u2028\u2029\u202A-\u202E\u2060-\u2069\u3164\uFEFF]/;

/**
 * Utility: strip markdown fences + reasoning blocks from a model-generated
 * one-line command and reject any string with Trojan-Source-class
 * invisible characters.
 *
 * Reasoning blocks: Gemma 4 (and other models with chat templates that
 * trigger a "thinking" mode) emit `<think>...</think>` before the final
 * answer. Those tokens aren't shell commands; we drop them.
 *
 * Markdown fences: models occasionally wrap single-line commands in
 * ```` ```bash ... ``` ```` despite instructions to the contrary. We
 * unwrap to the inner line.
 *
 * Returns the cleaned command string, or `null` if the model's output
 * is rejected (empty or contains dangerous invisibles).
 */
export function extractCommand(raw: string): string | null {
    let s = raw.trim();
    s = s.replace(/<think>[\s\S]*?<\/think>/g, "").trim();
    s = s.replace(/^```[a-zA-Z0-9_-]*\s*\n?/, "");
    s = s.replace(/\n?```\s*$/, "");
    const line = s
        .split("\n")
        .map((l) => l.trim())
        .find((l) => l.length > 0);
    const cmd = (line ?? s).trim();
    if (!cmd) return null;
    if (DANGEROUS_INVISIBLE_CHARS.test(cmd)) {
        return null;
    }
    return cmd;
}

/**
 * Re-exported for call sites that need to surface the rejection reason
 * (AI panel "Run fix" → "extractFixCommand" → user-visible error). Kept
 * as a separate function rather than a single `Result`-like return so
 * the ~95% common case (clean output) doesn't pay an object-alloc tax.
 */
export function hasDangerousInvisibles(s: string): boolean {
    return DANGEROUS_INVISIBLE_CHARS.test(s);
}

/**
 * SECURITY: patterns that mark an AI-suggested command as potentially
 * destructive. When `classifyRisk` returns non-null, the AI panel's Run
 * button is styled differently and requires a second explicit click
 * (with the matched pattern surfaced in a warning banner) before the
 * command reaches the PTY.
 *
 * The list is deliberately conservative — false positives cost one
 * extra click; false negatives let a prompt-injected `rm -rf ~` land
 * with the same UX as `ls`. Ordered so the FIRST match is the most
 * informative reason to show the user.
 */
interface RiskPattern {
    re: RegExp;
    reason: string;
}

const RISK_PATTERNS: RiskPattern[] = [
    { re: /:\(\s*\)\s*\{/, reason: "fork-bomb pattern" },
    { re: /\brm\b.*(-[a-z]*[rf]|--recursive|--force)/i, reason: "recursive/force rm" },
    { re: /\brm\s+-[rf]+[a-z]*\s/i, reason: "rm with -r/-f flags" },
    { re: /\brm\s+\/(?!tmp\b|var\/tmp\b)/i, reason: "rm targeting a root-level path" },
    { re: /\bsudo\b/, reason: "sudo privilege escalation" },
    { re: /\b(dd|mkfs|fdisk|shred|wipefs)\b/, reason: "raw disk / filesystem mutator" },
    { re: /\bchmod\s+[-+]?[0-9]*[0-9][0-9][0-9]/, reason: "octal chmod" },
    { re: /\bchown\b/, reason: "chown" },
    { re: /\|\s*(sh|bash|zsh|fish)\b/, reason: "piping into a shell" },
    { re: /\b(curl|wget|fetch)\b.*\|\s*(sh|bash|zsh)/i, reason: "curl-pipe-to-shell" },
    { re: />\s*\/dev\/(?!null\b|stderr\b|stdout\b|tty\b)/, reason: "redirect to a device" },
    { re: />\s*\/etc\//, reason: "redirect into /etc" },
    { re: /\b(killall|pkill)\s+-9/, reason: "force-kill" },
    { re: /\bgit\s+(push|reset|clean)\s+.*(--force|-f\b|--hard)/i, reason: "destructive git flag" },
    { re: /\bnpm\s+(unpublish|publish)\b/, reason: "npm publish/unpublish" },
    { re: /\bdocker\s+(rm|rmi|system\s+prune|volume\s+rm)/, reason: "docker destructive op" },
];

/**
 * Run all patterns against `cmd`, return the first match's reason.
 * `null` = command passes the heuristic with no warning. Not a
 * security boundary on its own — social engineering can still get
 * a careless user to click twice — but it raises the friction floor
 * for the most common attack payloads.
 */
export function classifyRisk(cmd: string): string | null {
    for (const pat of RISK_PATTERNS) {
        if (pat.re.test(cmd)) {
            return pat.reason;
        }
    }
    return null;
}
