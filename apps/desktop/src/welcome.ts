/**
 * Welcome banner for fresh sessions.
 *
 * Printed directly into the xterm buffer (via terminal.writeRaw) instead
 * of painted as a DOM overlay. Reasons:
 *   1. It naturally scrolls out of view when the user runs a command —
 *      no "hide the welcome" logic needed.
 *   2. It respects the active theme because xterm is already theme-aware.
 *   3. It doesn't steal keyboard focus or interact with overlays.
 *
 * Content budget: ~15 lines. First impression counts; anything longer
 * feels like a README crowbarred into the terminal.
 */

import type { TerminalHandle } from "./terminal";

/**
 * Write the welcome banner into `handle`'s xterm buffer. Call this after
 * `setupTerminal` resolves and before the shell prints anything — the
 * banner sits above the first prompt. Safe to call once per session.
 */
export function writeWelcome(handle: TerminalHandle): void {
    const ACCENT = "\x1b[38;2;79;195;247m"; // sky-blue (dark theme accent)
    const DIM = "\x1b[2m";
    const BOLD = "\x1b[1m";
    const RESET = "\x1b[0m";

    // ArcTerm ASCII wordmark. Kept tight so small windows don't wrap it.
    const banner = `${BOLD}${ACCENT}ArcTerm${RESET} ${DIM}— AI-powered terminal${RESET}`;

    const rows: Array<[string, string]> = [
        ["⌘K", "Ask AI to write a command"],
        ["? <query>", "AI shortcut — type in the input, hit Enter"],
        ["⌘⇧E", "Explain the last error (or current input)"],
        ["⌘T / ⌘W", "New session / close session"],
        ["⌘1 – ⌘9", "Switch to session N"],
        ["↑ / Ctrl+R", "Browse / search command history"],
        ["Tab", "Complete path / subcommand / option"],
        ["/arcterm-help", "List ArcTerm slash-commands"],
    ];

    // Widest key column decides the padding so the description column
    // aligns visually without relying on tab stops (which vary across
    // fonts inside xterm).
    const keyWidth = Math.max(...rows.map(([k]) => k.length));

    handle.writeRaw(`\r\n ${banner}\r\n\r\n`);
    for (const [key, desc] of rows) {
        const paddedKey = key.padEnd(keyWidth, " ");
        handle.writeRaw(
            ` ${ACCENT}${paddedKey}${RESET}  ${DIM}${desc}${RESET}\r\n`,
        );
    }
    handle.writeRaw("\r\n");
}
