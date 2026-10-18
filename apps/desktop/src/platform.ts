/**
 * Platform keyboard conventions.
 *
 * ArcTerm's app-level shortcuts were born as ⌘-chords on macOS. Off macOS
 * the Meta key is the Super/Win key, which the OS claims for itself (Win+T
 * focuses the taskbar, Win+W opens widgets), so the same chords never reach
 * the app. Plain Ctrl is not a substitute either: Ctrl+T/W/K/E/F all mean
 * something to a shell line editor and to TUIs (Ctrl+W deletes a word in
 * PSReadLine and bash, Ctrl+K kills to end of line, vim uses Ctrl+W for
 * windows), and this app forwards raw keystrokes to those programs.
 *
 * So off macOS we follow the Windows Terminal / GNOME Terminal convention:
 * Ctrl+Shift+<letter> for app actions, Alt+<digit> to pick a session,
 * Ctrl+Tab / Ctrl+Shift+Tab to cycle, Ctrl+, for settings. Ctrl+Shift+<letter>
 * is unbound in PSReadLine, readline and zle by default.
 *
 * Every match is on `ev.code` (physical key), not `ev.key`: with Shift held
 * `ev.key` turns `[` into `{` and `t` into `T`, and non-US layouts move
 * letters around; the physical key is stable.
 */

export const isMac: boolean = /Mac|iPhone|iPad|iPod/.test(navigator.platform);

export type AppShortcut =
  | { kind: "new-session" }
  | { kind: "close-session" }
  | { kind: "switch-session"; ordinal: number }
  | { kind: "next-session" }
  | { kind: "prev-session" }
  | { kind: "settings" }
  | { kind: "global-search" }
  | { kind: "ai-command" }
  | { kind: "ai-explain" };

/** Classify a keydown as one of ArcTerm's app-level shortcuts, or null. */
export function matchAppShortcut(ev: KeyboardEvent): AppShortcut | null {
  return isMac ? matchMac(ev) : matchCtrlShift(ev);
}

function matchMac(ev: KeyboardEvent): AppShortcut | null {
  // Alt is left to the editor (Option+← is word-jump in WebKit).
  if (!ev.metaKey || ev.altKey) return null;
  const digit = digitOrdinal(ev);
  if (!ev.shiftKey) {
    if (ev.code === "KeyT") return { kind: "new-session" };
    if (ev.code === "KeyW") return { kind: "close-session" };
    if (digit !== null) return { kind: "switch-session", ordinal: digit };
    if (ev.code === "Comma") return { kind: "settings" };
    if (ev.code === "KeyK") return { kind: "ai-command" };
    return null;
  }
  // ⌘⇧] / ⌘⇧[ — next / previous session (VS Code convention).
  if (ev.code === "BracketRight") return { kind: "next-session" };
  if (ev.code === "BracketLeft") return { kind: "prev-session" };
  if (ev.code === "KeyF") return { kind: "global-search" };
  if (ev.code === "KeyE") return { kind: "ai-explain" };
  return null;
}

function matchCtrlShift(ev: KeyboardEvent): AppShortcut | null {
  if (ev.metaKey) return null;
  // Alt+<digit>: pick a session. AltGr on European layouts reports as
  // Ctrl+Alt, which the `!ev.ctrlKey` guard keeps out of this branch.
  if (ev.altKey && !ev.ctrlKey && !ev.shiftKey) {
    const digit = digitOrdinal(ev);
    return digit === null ? null : { kind: "switch-session", ordinal: digit };
  }
  if (!ev.ctrlKey || ev.altKey) return null;
  if (ev.code === "Tab") return { kind: ev.shiftKey ? "prev-session" : "next-session" };
  if (!ev.shiftKey) {
    return ev.code === "Comma" ? { kind: "settings" } : null;
  }
  switch (ev.code) {
    case "KeyT":
      return { kind: "new-session" };
    case "KeyW":
      return { kind: "close-session" };
    case "KeyF":
      return { kind: "global-search" };
    case "KeyK":
      return { kind: "ai-command" };
    case "KeyE":
      return { kind: "ai-explain" };
    default:
      return null;
  }
}

function digitOrdinal(ev: KeyboardEvent): number | null {
  const m = /^Digit([1-9])$/.exec(ev.code);
  return m ? Number.parseInt(m[1], 10) : null;
}

/**
 * Human-readable chord names for the welcome banner and help text, keyed
 * by what the user is trying to do rather than by key so the two platform
 * tables cannot drift apart.
 */
export const shortcutLabels: Record<
  "aiCommand" | "aiExplain" | "newClose" | "switch" | "globalSearch" | "settings",
  string
> = isMac
  ? {
      aiCommand: "⌘K",
      aiExplain: "⌘⇧E",
      newClose: "⌘T / ⌘W",
      switch: "⌘1 – ⌘9",
      globalSearch: "⌘⇧F",
      settings: "⌘,",
    }
  : {
      aiCommand: "Ctrl+Shift+K",
      aiExplain: "Ctrl+Shift+E",
      newClose: "Ctrl+Shift+T / W",
      switch: "Alt+1 – Alt+9",
      globalSearch: "Ctrl+Shift+F",
      settings: "Ctrl+,",
    };
