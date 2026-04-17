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

    const promise = new Promise<string>(async (resolve, reject) => {
        let requestId: string;
        try {
            requestId = await invoke<string>("ai_stream", { req });
        } catch (err) {
            settled = true;
            reject(err);
            return;
        }

        unlisten = await listen<AiChunk>("ai://chunk", (ev) => {
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
        });
    });

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
 * Utility: strip markdown fences from a model-generated one-line command.
 * The model is instructed not to emit them, but it still does occasionally.
 * We take the first non-empty line and trim shell-ish decoration.
 */
export function extractCommand(raw: string): string {
    let s = raw.trim();
    // Drop leading/trailing ``` fences with optional language tag.
    s = s.replace(/^```[a-zA-Z0-9_-]*\s*\n?/, "");
    s = s.replace(/\n?```\s*$/, "");
    // Take the first non-blank line.
    const line = s
        .split("\n")
        .map((l) => l.trim())
        .find((l) => l.length > 0);
    return (line ?? s).trim();
}
