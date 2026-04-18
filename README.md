# ArcTerm

> A modern terminal for macOS with built-in AI. Use a free local **Gemma 4**
> model (Metal-accelerated) or your existing **Claude CLI** subscription —
> or both, with automatic fallback.

ArcTerm takes the ideas that made Warp and Fig feel fresh — block-based
output, rich tab completion, a proper input editor — and makes them
open source, offline-capable, and yours. No login wall, no telemetry,
no subscription required (unless you want the Claude one).

**License:** Apache-2.0 (code) · CC BY 4.0 (name + icons).
**Platform:** macOS 11+ (Apple Silicon + Intel).
**Status:** v0.1.0 alpha — feature-complete but rough edges remain.

---

## What's in the box

**AI features**
- `⌘K` — ask Claude (or local Gemma) to write a shell command from natural language.
- `? <query>` — same thing, inline, no modal.
- `⌘⇧E` — explain the last error (or whatever you're typing).
- Three backends: Claude CLI, local Gemma 4, or **auto** (Claude first, Gemma on failure).
- Context-aware: every request ships with your cwd, shell, git branch, and the last 10 commands in the same directory.

**Terminal UX**
- Custom input editor with proper macOS keybindings (`⌘←/→`, `⌥←/→`, selection, multi-line with `⇧↵`).
- Block-based output: each command + its result is a visually bounded block with exit code and duration.
- `Tab` completion across **617 CLIs** (git, brew, docker, kubectl, cargo, npm, aws, and more — sourced from [Fig's spec library](https://github.com/withfig/autocomplete)).
- Ghost-text history autosuggestions (`→` to accept).
- `↑` / `Ctrl+R` for a scrollable, searchable history overlay.
- Multi-session sidebar with `⌘T` / `⌘W` / `⌘1-9` keybindings, inline rename, right-click context menu.
- Dark and light themes, switchable live.

**Shell integration**
- Works with **zsh**, **bash**, and **fish** — prompt suppressed, cwd tracked via `OSC 7`, command blocks via `OSC 133`.
- Chain-loads your existing `.zshrc` / `.bashrc` / `config.fish` so aliases, plugins, starship, oh-my-zsh — all of it — keep working.

**Model downloader**
- `/arcterm-download gemma-4-e2b-it-q4km` — stream a GGUF from HuggingFace with live progress, SHA-256 verification, and resume-on-crash.
- Ships with five Gemma 4 variants in the registry (E2B + E4B at multiple quantizations).

---

## Requirements

- macOS 11 (Big Sur) or newer, Apple Silicon or Intel.
- [Claude CLI](https://claude.com/claude-code) *(optional)* — for the Claude backend.
- A GPU of your Mac's generation — all Apple Silicon Macs and 2016+ Intel Macs with an eGPU work with Metal acceleration for local Gemma.

---

## Quick install

Download the latest `.dmg` from [Releases](https://github.com/vatsalraicha/ArcTerm/releases),
drag `ArcTerm.app` into `/Applications`, and launch.

On first launch macOS will warn "ArcTerm is from an unidentified developer" —
right-click the app → **Open** to bypass the warning once (the app isn't notarized
yet; that's a v1.0 item).

For build-from-source instructions, troubleshooting, and dev setup, see
[INSTALL.md](./INSTALL.md).

---

## First-run workflow

1. Launch ArcTerm. You'll see a welcome banner with the keyboard cheatsheet.
2. If you have the Claude CLI installed and logged in, `⌘K` just works — try it.
3. For local AI, run `/arcterm-download gemma-4-e2b-it-q4km` (the default; ~3.5 GB). After download completes it auto-loads and the mode flips to **auto** (Claude first, Gemma as fallback).
4. `/arcterm-help` lists every ArcTerm-specific slash command.
5. `⌘,` opens the settings panel for theme, backend mode, model selection, and Claude CLI path.

---

## Keyboard reference

| Shortcut | Action |
|---|---|
| `⌘T` / `⌘W` | New / close session |
| `⌘1`…`⌘9` | Switch session by ordinal |
| `⌘⇧[` / `⌘⇧]` | Prev / next session |
| `⌘K` | AI panel — generate a command from natural language |
| `? <query>` | Same, inline |
| `⌘⇧E` | AI panel — explain the last error (or editor contents) |
| `⌘,` | Settings panel |
| `⌘F` | Search within the current terminal buffer |
| `↑` / `Ctrl+R` | History overlay (browse / search) |
| `→` | Accept ghost-text history suggestion |
| `Tab` | Completion dropdown (FS paths + CLI subcommands/options) |
| `⇧↵` | Newline in editor |
| `↵` | Execute command |
| `⌃C` | Send SIGINT to the running process |
| `Esc` | Close overlay / clear input |

---

## Slash commands

All internal commands share the reserved `/arcterm-` prefix — they never hit
the shell.

| Command | Purpose |
|---|---|
| `/arcterm-help` | List all slash commands |
| `/arcterm-status` | Show active AI backend + loaded model |
| `/arcterm-model [claude\|local\|auto]` | Show or set AI backend |
| `/arcterm-models` | List registered models (installed + available) |
| `/arcterm-download <id>` | Download a model from the registry |
| `/arcterm-load <id>` | Swap the active local model |
| `/arcterm-theme [dark\|light]` | Show or set theme |

---

## Configuration

User settings live at `~/.arcterm/config.json` (auto-created on first run):

```json
{
  "theme": "dark",
  "ai": {
    "mode": "auto",
    "localModel": "gemma-4-e2b-it-q4km",
    "claudePath": ""
  }
}
```

Everything here is also accessible via `⌘,` (settings panel) or slash commands.

Other runtime files under `~/.arcterm/`:

- `shell-integration/` — the zsh/bash/fish hooks (auto-managed).
- `zdotdir/` — zsh `ZDOTDIR` chain-load files (auto-managed).
- `models/` — downloaded GGUFs.
- `history.db` — command history (SQLite).

---

## Tech stack

- [Tauri v2](https://v2.tauri.app/) — Rust backend, WebKit frontend
- [xterm.js](https://xtermjs.org/) with the WebGL renderer
- [portable-pty](https://github.com/wez/wezterm/tree/main/pty) for PTY management
- [llama-cpp-2](https://github.com/utilityai/llama-cpp-rs) for local inference (with Metal on Apple Silicon)
- [rusqlite](https://github.com/rusqlite/rusqlite) (bundled SQLite) for history
- TypeScript + vanilla DOM on the frontend — no framework

---

## Credits

ArcTerm is created and maintained by **[Vatsal Raicha](https://github.com/vatsalraicha)**.
If you fork this project, vendor parts of it, or build a derivative product on
top of it, please keep the attribution in [`LICENSE`](./LICENSE) and
[`NOTICE`](./NOTICE) and link back to the upstream repository — it's both a
license requirement and how the project grows.

- **Tab-completion specs** are imported from
  [`withfig/autocomplete`](https://github.com/withfig/autocomplete) (MIT).
  617 of their ~735 command specs ship compiled-in; the remainder use
  TypeScript patterns our AST importer doesn't handle yet.
- **Gemma 4** weights by Google DeepMind, Apache-2.0. The registry points
  at [bartowski's GGUF quantizations](https://huggingface.co/bartowski).
- **Built with** [Claude Code](https://claude.com/claude-code) — iteratively,
  over about a week of focused sessions.

---

## Contributing

Early days and the code moves fast, but pull requests are welcome.
Before opening one:

1. Read [`CLAUDE.md`](./CLAUDE.md) — the architecture handbook. It'll get
   you oriented in ~10 minutes.
2. Run `cargo check`, `cargo clippy -D warnings`, `cargo test`, and
   `pnpm vite:build` locally — CI enforces all four.
3. Commit messages use Conventional Commits (`feat:`, `fix:`, `docs:`, etc.).

---

## License

ArcTerm is dual-licensed to draw a clean line between the code and the brand:

- **Source code** — [Apache License 2.0](./LICENSE). You can use, modify,
  redistribute, and ship commercial derivatives as long as you keep the
  copyright notice, the [`NOTICE`](./NOTICE) file, and note any changes you
  made. Apache 2.0 also grants you a patent license from contributors.
- **Name + icons** ("ArcTerm" wordmark, app icons under
  `apps/desktop/src-tauri/icons/`) — [Creative Commons Attribution 4.0
  International (CC BY 4.0)](./LICENSE-BRAND.md). Reuse freely with credit
  to Vatsal Raicha + a link back to this repo.

If you're shipping a distinctly different product built on ArcTerm, please
rename it and use your own icons — that keeps things clear for users.

Copyright © 2026 Vatsal Raicha.
