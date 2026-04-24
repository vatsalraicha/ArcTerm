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
 * M-6 expanded the list beyond the original hand-enumerated set to
 * reject every Unicode format (Cf) and control (Cc) character except
 * the three shell-legitimate whitespace forms (\t, \n, \r). This
 * subsumes the old enumeration and additionally catches:
 *
 *   - Tag characters U+E0020–E007F (invisible prompt-injection carriers,
 *     widely abused since ~2023).
 *   - Variation selectors U+FE00–FE0F, U+E0100–E01EF (covert encoders).
 *   - Mongolian Free Variation Selectors U+180B–180D.
 *   - Language tag U+E0001.
 *   - Any future Cf / Cc codepoint added by a later Unicode version.
 *
 * On detection we reject the command outright — `extractCommand` and
 * `extractFixCommand` return `null`. Callers surface a user-visible
 * error instead of proposing the command; never silently strip, since
 * attackers could craft the remainder to still be destructive even
 * after the invisibles are removed.
 */
const DANGEROUS_INVISIBLE_CHARS =
    /[\p{Cf}\u2028\u2029]|[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/u;

/**
 * SECURITY: for the command-normalization path in `classifyRisk`, the
 * bar is lower — we just need to collapse tricks like NBSP into plain
 * whitespace and strip obvious zero-widths so the regex pass sees the
 * "real" command. Shell-legitimate whitespace (space, tab) survives.
 */
const INVISIBLE_NOT_WHITESPACE =
    /[\p{Cf}\u200B-\u200F\u2028\u2029\u2060-\u2064\u2066-\u206F\uFEFF]/gu;

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
    // H-3: copying/streaming onto a raw disk-device is as dangerous as
    // `dd`; both of these flags passing are worse than `rm`.
    { re: /\bdd\b[\s\S]*?\bof=\/dev\//, reason: "dd onto /dev device" },
    { re: /\bcp\b[\s\S]*?\s\/dev\/(?!null\b|stdout\b|stderr\b|tty\b)/, reason: "cp to a /dev device" },
    { re: /\bchmod\s+[-+]?[0-9]*[0-9][0-9][0-9]/, reason: "octal chmod" },
    // H-3: chmod -R ... 000/0000 effectively locks users out of their own files.
    { re: /\bchmod\s+(?:-R|--recursive)\b.*\b0*0{3,}\b/, reason: "recursive chmod to 000" },
    { re: /\bchown\b/, reason: "chown" },
    // H-3: recursive chown on a user tree is a classic "lock yourself out"
    // payload even without a privilege-escalation (passed to sudo, it's
    // catastrophic; without sudo it still trashes ownership of user files).
    { re: /\bchown\s+(?:-R|--recursive)\b/, reason: "recursive chown" },
    { re: /\|\s*(sh|bash|zsh|fish)\b/, reason: "piping into a shell" },
    { re: /\b(curl|wget|fetch)\b.*\|\s*(sh|bash|zsh)/i, reason: "curl-pipe-to-shell" },
    { re: />\s*\/dev\/(?!null\b|stderr\b|stdout\b|tty\b)/, reason: "redirect to a device" },
    { re: />\s*\/etc\//, reason: "redirect into /etc" },
    // H-3: truncation of HOME-anchored paths. `>`, `>|` (zsh noclobber
    // override), and `>>` are all covered. Space after the redirect
    // operator is optional in POSIX shells.
    { re: /(^|[\s;&|(])>\|?\s*~/, reason: "redirect-truncate into $HOME" },
    { re: /(^|[\s;&|(])>\|?\s*\$HOME\b/, reason: "redirect-truncate into $HOME" },
    { re: /(^|[\s;&|(])>\|?\s*\$\{HOME/, reason: "redirect-truncate into $HOME" },
    // H-3: `find … -delete` / `find … -exec rm` are the two most common
    // destructive find uses. Both have well-documented footgun history.
    { re: /\bfind\b[\s\S]*?-delete\b/, reason: "find -delete" },
    { re: /\bfind\b[\s\S]*?-exec\s+(rm|shred|mv)\b/, reason: "find -exec rm/shred/mv" },
    // H-3: `mv <something> /dev/null` deletes the source (it rename(2)s
    // onto the device node, and bash+zsh happily follow).
    { re: /\bmv\s+[^|;&]*\s+\/dev\/(null|zero)\b/, reason: "mv into /dev/null" },
    { re: /\b(killall|pkill)\s+-9/, reason: "force-kill" },
    { re: /\bgit\s+(push|reset|clean)\s+.*(--force|-f\b|--hard)/i, reason: "destructive git flag" },
    { re: /\bnpm\s+(unpublish|publish)\b/, reason: "npm publish/unpublish" },
    { re: /\bdocker\s+(rm|rmi|system\s+prune|volume\s+rm)/, reason: "docker destructive op" },
];

/**
 * SECURITY (H-3 defense-in-depth): any AI-sourced command containing
 * shell metacharacters gets flagged even if no explicit rule matches.
 * This catches the "unknown unknown" destructive combinator we haven't
 * yet written a rule for — prompt-injection payloads that chain safe
 * primitives into something dangerous (`curl X && rm Y`, subshell
 * exfiltration, etc.). Users can still click through the confirm
 * dialog; we just refuse to treat it as low-risk by default.
 */
const SHELL_METACHARS_RE = /[;&|`]|\$\(|&&|\|\||>>|<\(/;

/**
 * Run all patterns against `cmd`, return the first match's reason.
 * `null` = command passes the heuristic with no warning. Not a
 * security boundary on its own — social engineering can still get
 * a careless user to click twice — but it raises the friction floor
 * for the most common attack payloads.
 *
 * M-5: normalize the input before regex matching. Heuristic command
 * classification is trivially bypassed when the attacker substitutes a
 * non-ASCII look-alike whitespace (NBSP, thin-space, ideographic space)
 * or a fullwidth Latin form for a letter. Apply NFKC to collapse
 * look-alikes and replace every non-ASCII whitespace with a plain
 * space before running the patterns. Bidi overrides are stripped by
 * `DANGEROUS_INVISIBLE_CHARS` upstream (extractCommand rejects first).
 */
function normalizeForRiskCheck(cmd: string): string {
    let s = cmd;
    try {
        s = s.normalize("NFKC");
    } catch {
        // Defensive — String.prototype.normalize only throws for bad forms.
    }
    // Strip zero-width / format controls that pass as no-chars visually.
    s = s.replace(INVISIBLE_NOT_WHITESPACE, "");
    // Collapse Unicode whitespace to a single ASCII space so `\s` /
    // `\b` in our patterns behave as humans expect.
    s = s.replace(/\p{White_Space}/gu, " ");
    return s;
}

export function classifyRisk(cmd: string): string | null {
    const normalized = normalizeForRiskCheck(cmd);
    for (const pat of RISK_PATTERNS) {
        if (pat.re.test(normalized)) {
            return pat.reason;
        }
    }
    // H-3 defense-in-depth: any shell metacharacter in an AI-sourced
    // command means "not safe to auto-run." Frame it softly so the UI
    // can still present a confirm rather than a hard rejection.
    if (SHELL_METACHARS_RE.test(normalized)) {
        return "shell metacharacter — confirm before running";
    }
    return null;
}
