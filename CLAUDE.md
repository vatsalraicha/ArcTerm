# ArcTerm — context brief for Claude

> **Purpose of this file.** You are Claude, starting a new session on this
> codebase. Read this first. It is the shortest path from zero to shipping
> changes in ArcTerm. Hard rules for this repo live in `Dos_And_Donts.md`
> (gitignored locally); summary at the bottom of this file.

## Orient yourself in 60 seconds

```bash
# What shipped and when
git log --oneline -20
git tag -l                          # v0.1.0-alpha.phase{1..7} exist

# Current runtime state on this machine
cat ~/.arcterm/config.json          # user settings: theme, ai.mode, localModel
ls ~/.arcterm/models/               # installed GGUFs
ls ~/.arcterm/{zdotdir,shell-integration}/   # shell hooks on disk

# Build health
cd apps/desktop/src-tauri && cargo check    # Rust
cd apps/desktop && pnpm vite:build          # Frontend
```

Latest commit (at the time of writing this doc): **`ad4a3e0`** —
`chore: retire Fig import infrastructure, bundle.json is now source
of truth`. The phase-8 security series (audit → waves 1–5 lite) has
landed; community paperwork is complete; the Fig importer has been
retired now that upstream is abandoned. The `pre-fig-removal` git
tag preserves the importer for future recovery if ever needed. Most
recent functional security work shipped as tag `v0.1.0-alpha.phase8.6`.

---

## What ArcTerm is

Modern terminal emulator inspired by Warp, with Claude CLI + local Gemma
as AI backends. **Open source, dual-licensed:** source code under Apache
2.0 (`LICENSE`), brand assets (the "ArcTerm" name + app icons) under
CC BY 4.0 (`LICENSE-BRAND.md`). Originally MIT through the phase-7
alpha; switched on 2026-04-18 for Apache's explicit patent grant,
change-notice clause, and NOTICE-file propagation — stronger
attribution than MIT while staying permissive. Two deliverables per
the spec:

1. **Desktop app** (shipped, what you work on daily) — Tauri v2 shell
   hosting xterm.js, custom input editor, sidebar with multi-session
   support, AI features, and slash-commands.
2. **VS Code extension** (deferred to Phase 6, not started) — same core
   TypeScript modules reused inside a webview panel.

Owner / git identity: **Vatsal Raicha** (`raicha@vatsallabs.com`).
Repo: `github.com/vatsalraicha/ArcTerm`.

The original requirements doc is `Project_Prompt_Detailed.md` in the repo
root (gitignored, kept locally). `Dos_And_Donts.md` lives beside it, also
gitignored — both are reference notes, not published.

---

## Tech stack

| Layer | Choice | Why |
|---|---|---|
| App shell | Tauri v2 | Rust backend, WebKit frontend, small binary vs Electron |
| Frontend | TypeScript + vanilla DOM | No framework, keeps terminal path fast |
| Terminal | xterm.js + WebGL renderer | Industry standard, truecolor, fast |
| PTY | portable-pty (Rust) | Cross-platform; one thread per PTY |
| Storage | rusqlite (bundled) | Command history, WAL mode |
| Async | tokio | Subprocess (Claude CLI) + streaming inference |
| Local LLM | llama-cpp-2 0.1 with `metal` | Apple Silicon GPU; auto-enabled on aarch64-darwin |
| HTTP | reqwest (rustls) | Model downloader, streaming + Range resume |
| Package mgmt | pnpm workspaces | `pnpm install` at repo root |
| Target | **macOS only** | Linux/Windows explicitly scoped out for alpha |

Not used: React/Vue/Svelte, Electron, Python (original spec suggested a
Python sidecar; dropped in favor of Rust subprocess calls).

---

## Repo layout

```
ArcTerm/
├── CLAUDE.md                       ← this file
├── README.md                       public-facing intro, build + install instructions
├── INSTALL.md                      end-user install guide for the .dmg
├── LICENSE                         Apache 2.0 (verbatim from apache.org)
├── LICENSE-BRAND.md                CC BY 4.0 covering "ArcTerm" name + icons
├── NOTICE                          Apache §4(d) attribution; third-party components
├── CODE_OF_CONDUCT.md              Contributor Covenant 2.1
├── CONTRIBUTING.md                 scope, build, PR + commit conventions
├── SECURITY.md                     private disclosure policy (GitHub advisories)
├── .github/
│   ├── workflows/
│   │   ├── ci.yml                  check on push + PR (cargo check/clippy/test + vite)
│   │   └── release.yml             on v* tag: build .app + .dmg, attach to draft release
│   └── ISSUE_TEMPLATE/
│       ├── bug_report.yml          structured YAML form: version, macOS, chip, repro
│       ├── feature_request.yml     problem-first form; flags "willing to PR"
│       └── config.yml              disables blank issues; routes Q&A → Discussions
├── apps/
│   └── desktop/
│       ├── package.json
│       ├── vite.config.ts          root=src/, outDir=../dist
│       ├── src/                    TypeScript frontend
│       │   ├── main.ts             bootstrap: wires everything
│       │   ├── terminal.ts         xterm + PTY bridge, OSC handlers, block separators
│       │   ├── input-editor.ts     contenteditable editor, keybindings, ghost text
│       │   ├── session-manager.ts  multi-PTY state + events
│       │   ├── sidebar.ts          session list, context menu, inline rename
│       │   ├── ai.ts               Claude/Local IPC wrappers, extractCommand
│       │   ├── ai-panel.ts         ⌘K / ⌘⇧E floating panel
│       │   ├── settings-panel.ts   ⌘, modal (theme, AI mode, local model, claude path)
│       │   ├── arcterm-commands.ts /arcterm-* slash command handler
│       │   ├── history-overlay.ts  ↑ / Ctrl+R searchable overlay
│       │   ├── completion-overlay.ts  Tab dropdown
│       │   ├── welcome.ts          per-session keyboard cheatsheet
│       │   └── styles/             one CSS file per component; tokens in main.css
│       └── src-tauri/
│           ├── Cargo.toml
│           ├── tauri.conf.json
│           ├── build.rs
│           ├── icons/              generated from icons/source.png by `tauri icon`
│           ├── completion-specs/   bundle.json = 617 Fig specs
│           ├── capabilities/default.json
│           └── src/
│               ├── lib.rs          app bootstrap, Tauri builder, menu
│               ├── main.rs         shim → lib::run()
│               ├── pty.rs          PtyManager, reader threads, OSC-passthrough
│               ├── ipc.rs          every #[tauri::command] handler
│               ├── history.rs      SQLite history store, autosuggest, search
│               ├── shell_hooks.rs  zsh ZDOTDIR install + bash rcfile generation
│               ├── settings.rs     ~/.arcterm/config.json, atomic write
│               ├── models/
│               │   ├── mod.rs      REGISTRY, cleanup_stranded_parts()
│               │   └── downloader.rs  streaming HTTP + Range resume + SHA verify
│               ├── ai/
│               │   ├── mod.rs      AiBackend trait, AiRequest/Response/Chunk
│               │   ├── context.rs  to_prompt_block + to_compact_prompt_block
│               │   ├── claude.rs   ClaudeCliBackend (subprocess claude -p)
│               │   ├── local_llama.rs  LocalLlamaBackend (llama-cpp-2, Metal)
│               │   ├── auto.rs     AutoBackend (Claude → fallback to local)
│               │   └── router.rs   AiRouter, Mode enum, runtime swap
│               └── completion/
│                   ├── mod.rs      dispatch: spec → fs fallback
│                   ├── fs.rs       filesystem completion, shell_escape + unescape
│                   └── specs.rs    Command-spec registry, lookup by root command
├── shell-integration/              included in binary via include_str!
│   ├── arcterm.zsh                 prompt suppression, OSC 7/133/1337 emitters
│   ├── arcterm.bash                bash-preexec-style hooks
│   ├── arcterm.fish                native fish events
│   └── zdotdir/{.zshenv,.zshrc}    chain-load user rc files, source arcterm.zsh
├── pnpm-workspace.yaml             apps/* + packages/*
├── package.json                    workspace root
└── .gitignore                      .claude/, briefs
```

Not in the repo but expected on dev machine:
`~/.arcterm/` (runtime state), `.fig-autocomplete-source/` (cloned on demand
for spec regeneration, gitignored), `apps/desktop/src-tauri/target/` (Rust).

---

## Phase history (tags)

| Tag | Scope |
|---|---|
| `v0.1.0-alpha.phase1` | Tauri shell + portable-pty + xterm.js basics |
| `v0.1.0-alpha.phase2` | Custom input editor + zsh ZDOTDIR integration |
| `v0.1.0-alpha.phase3` | Blocks (OSC 133) + history (SQLite) + autosuggest + overlay |
| `v0.1.0-alpha.phase4` | SessionManager, sidebar, multi-PTY tabs, ⌘T/W/1-9 |
| `v0.1.0-alpha.phase5a` | AiBackend trait + ClaudeCliBackend + ⌘K/⌘⇧E |
| `v0.1.0-alpha.phase5b` | LocalLlamaBackend (Gemma 4, Metal) + AutoBackend + /arcterm-* |
| `v0.1.0-alpha.phase7` | Polish batch (theme, CI, fig specs, icon, menu, many fixes) |
| `v0.1.0-alpha.phase8` | First security audit + 14 fixes (CSP, fs perms, IPC hardening, …) |
| `v0.1.0-alpha.phase8.1` | GGUF SHA pinning, IPC capability trim, OSC 133/1337 nonce auth |
| `v0.1.0-alpha.phase8.2` | Post-audit wave 1: history sanitation, claudePath validation, 0600 GGUFs |
| `v0.1.0-alpha.phase8.3` | Waves 2 + 2.5: verify-on-load, OSC 52/8 hardening, deferred boot-load + progress UI |
| `v0.1.0-alpha.phase8.4` | Wave 3: AI-output Unicode rejection + destructive-command confirm + history exclusion |
| `v0.1.0-alpha.phase8.5` | Wave 4: GGUF perm sweep, CSP dev/prod doc, Fig bundle sanity test, audit triage header |
| `v0.1.0-alpha.phase8.6` | Wave 5 (lite): in-memory IPC audit log + pty_write sniffer + /arcterm-audit |
| `pre-fig-removal` | Snapshot of scripts/import-fig-specs.mjs etc. before it was retired on main |

Phase 6 (VS Code extension) intentionally skipped. Current plan: **NOT a
v1.0 blocker** — ship v1.0 as desktop app first, extension can come as
v1.x. Phase 7 is mostly done (see "Phase 7 status" below); the tail is
folded into the beta roadmap.

---

## Architecture at a glance

### Backend process graph

```
main.rs → lib.rs::run()
  ├── shell_hooks::install()       writes ~/.arcterm/{zdotdir,shell-integration}/...
  ├── models::cleanup_stranded_parts()   sweeps stale .part files on boot
  ├── HistoryStore::open()         opens ~/.arcterm/history.db (WAL)
  ├── SettingsStore::open()        reads ~/.arcterm/config.json
  ├── [eager-load LocalLlamaBackend if mode ∈ {Local, Auto}]
  ├── AiRouter::new(claude, local?, mode)
  ├── PtyManager::new(shell_paths)
  └── Tauri::Builder
        .setup(build menu: ArcTerm → Settings…, Edit, Window)
        .on_menu_event(arcterm:settings → emit "menu://settings")
        .manage(PtyManager, AiRouter, SettingsStore, DownloadLock, HistoryStore)
        .invoke_handler(pty_*, history_*, ai_*, fs_complete, settings_*, model_*)
        .run()
```

### Frontend bootstrap (main.ts)

```
DOMContentLoaded
  → boot(mounts)
      ├── new SessionManager(stackHost)
      ├── readSavedTheme() + applyTheme()            class on <html>
      ├── new HistoryOverlay(...)
      ├── new SettingsPanel(...)
      ├── listen("menu://settings") → panel.open()
      ├── new Sidebar(manager, listEl, ...)
      ├── aiAvailable = await aiIsAvailable()
      ├── new AiPanel(...)                            ⌘K / ⌘⇧E
      ├── global keydown: ⌘T/W/1-9/[/], ⌘,, ⌘K, ⌘⇧E
      ├── new InputEditor(onSubmit, suggestFor, completeFor, showCompletions)
      └── await manager.create()                      first session
```

### Key data flows

- **Command submit (shell):** editor.onSubmit → main.submitCommand →
  `writeBlockStart(cwd, branch)` (top pill ONLY, no command echo — zle
  echoes for us) → `history_insert` IPC → PTY write.
- **Block end:** shell's precmd emits `OSC 133;D;<exit>` → terminal.ts
  OSC handler → session.onCommandEnd → `writeBlockEnd` (separator + ✓/✗
  + duration) → `history_update_exit` IPC.
- **⌘K:** AiPanel.openForCommand → aiAsk → extractCommand → Run/Edit.
- **`?` prefix:** main.runAiShortcut → aiAsk → `editor.setValue(cmd)`.
  **Must NOT write to xterm** — earlier writeRaw attempts raced with
  xterm's async parse queue and broke submission (fix: `a8ab2de`).
- **Tab:** InputEditor.runCompletion → `fs_complete` IPC → if single
  result splice inline, else open CompletionOverlay. The spec router
  dispatches to Fig registry when first token matches a known command.

---

## AI backend

Backends implement `trait AiBackend` (`ai/mod.rs`). All three types
present; `AiRouter` holds `Arc<dyn AiBackend>` and swaps atomically via
`set_mode(Mode::{Claude, Local, Auto})`.

- **ClaudeCliBackend** — subprocess `claude -p <prompt> --output-format
  json|stream-json`. `base_command()` strips all Anthropic env vars
  (ANTHROPIC_API_KEY and several siblings) so subscription auth wins.
- **LocalLlamaBackend** — llama-cpp-2; loads GGUF from disk. Reads chat
  template from GGUF metadata via `apply_chat_template()`, falls back to
  hardcoded Gemma template on Jinja parser errors. Field order in the
  struct is load-bearing for clean shutdown (llama.cpp aborts if
  LlamaBackend drops before LlamaModel; comment in `local_llama.rs`).
- **AutoBackend** — Claude first; falls back to local on auth errors,
  timeouts, rate limits, network failures. Content-policy refusals pass
  through unchanged.

**Lazy model load:** `ai_set_mode` and `ai_set_local_model` both lazy-
load a GGUF if none is in memory, so users can switch into Local/Auto
mode after boot without restarting. The registry in `models/mod.rs`
knows Gemma 4 E2B (Q4_K_M, IQ2_M) and E4B (Q4_K_M, IQ2_M, Q8_0).

---

## Settings (`~/.arcterm/config.json`)

```json
{
  "theme": "dark" | "light",
  "ai": {
    "mode": "claude" | "local" | "auto",
    "localModel": "gemma-4-e2b-it-q4km" | ...,
    "claudePath": "" | "/absolute/path/to/claude"
  }
}
```

Missing fields default via serde; corrupt JSON falls back to defaults
with a log warning (never blocks boot). Writes are atomic (tempfile +
rename).

---

## Shell integration (`~/.arcterm/`)

- `zdotdir/.zshenv, .zshrc` chain-load user's own rc files, then source
  `shell-integration/arcterm.zsh`. pty.rs exports `ZDOTDIR` + marker
  `ARCTERM_SESSION=1`.
- Emits:
  - `OSC 7` on chpwd (cwd tracking → prompt-bar update)
  - `OSC 133;C` on preexec (command starting)
  - `OSC 133;D;<exit>` on precmd (block-end with exit code)
  - `OSC 1337;ArcTermBranch=<name>` on precmd (git branch chip)
- Prompt is blanked (`PROMPT=''`) so ArcTerm draws the whole UI.
- bash + fish equivalents exist (`arcterm.bash`, `arcterm.fish`) but
  user has only exercised zsh in practice. pty.rs dispatches on the
  shell basename.

---

## AI context building (`ai/context.rs`)

Every AI request is enriched with: cwd, shell, OS, git branch, last 10
same-cwd history commands, and (for explain flows) the failing command
+ captured stderr/stdout. Local backend uses the RICH prompt block same
as Claude — the Phase 5b attempt to strip context for small quants made
Gemma worse, not better. Lesson: don't over-tune prompts for specific
backends.

Per-block output capture: `terminal.ts::writeBlockEnd` slices xterm's
buffer between block-start and block-end absolute line indices, caps at
8 KB keeping the tail. Stored in `session.state.lastOutput` for use by
`⌘⇧E`.

---

## Keybindings & UX primitives

| Key | Action |
|---|---|
| `⌘T` | New session |
| `⌘W` | Close session (confirms if command running) |
| `⌘1`–`⌘9` | Switch session by ordinal |
| `⌘⇧[` / `⌘⇧]` | Prev / next session |
| `⌘K` | AI panel → command generation |
| `⌘⇧E` | AI panel → explain (uses last error or editor contents) |
| `⌘,` | Settings panel |
| `? <query>` | Inline AI command shortcut |
| `Tab` | Completion dropdown (FS paths + Fig subcommands/options) |
| `→` | Accept history ghost text |
| `↑` | History overlay (browse) |
| `Ctrl+R` | History overlay (search) |
| `Esc` | Close overlay / clear input |
| `Enter` | Submit command |
| `Shift+Enter` | Newline in editor |
| `Ctrl+C` | Send SIGINT to PTY |

Global menu: **ArcTerm → Settings…** (wired in `lib.rs::run()`,
emits `menu://settings` which main.ts listens for).

---

## Slash commands (reserved `/arcterm-` prefix)

Intercepted in `main.ts::onSubmit` before PTY send. Handled by
`arcterm-commands.ts::runInternalCommand`. For internal commands the
frontend writes the command manually via `writeRaw` after the pill
(since no shell + no zle echo).

| Command | Purpose |
|---|---|
| `/arcterm-help` | List all slash commands |
| `/arcterm-model [claude\|local\|auto]` | Show / set backend mode (lazy-loads local on demand) |
| `/arcterm-models` | List registry entries + installed state |
| `/arcterm-download <id>` | Stream download + SHA verify + auto-load |
| `/arcterm-load <id>` | Swap active local model without changing mode |
| `/arcterm-theme [dark\|light]` | Show / set UI theme |
| `/arcterm-status` | Show router state + active backend + loaded model variant |

---

## CI / release

- **`ci.yml`** — runs on every push to main + every PR. macOS only.
  `cargo check`, `cargo clippy -D warnings`, `cargo test` (incl.
  `--doc`), `pnpm vite:build`. Concurrency cancellation to avoid
  wasting runner time on superseded pushes.
- **`release.yml`** — runs on `v*` tag push. Matrix builds
  aarch64-apple-darwin + x86_64-apple-darwin via `tauri-apps/tauri-action@v0`.
  Creates a **draft** GitHub Release with `.dmg` artifacts attached;
  auto-flags pre-release for alpha/beta/rc tags. Review + publish manually.
- **`MACOSX_DEPLOYMENT_TARGET=11.0`** — set at workflow env level.
  llama-cpp-sys-2 uses `std::filesystem::path` which the Xcode SDK marks
  as introduced in macOS 10.15; older targets fail to compile.
- **Linux/Windows intentionally omitted.** If we ever re-add, need
  target-gated llama-cpp-2 (metal only on macOS) + platform-specific
  deps (webkit2gtk on Linux, webview2 on Windows).
- No notarization yet; users will see "unidentified developer" on first
  launch. Real signing is a v1.0 item ($99/year Apple Developer account).

---

## Known non-trivial design decisions

1. **Drop our own command echo; let zle's echo be the visible command.**
   Earlier we rendered `❯ <command>` ourselves AND zle echoed when
   reading — visible duplicate. We tried hiding zle's echo via
   conceal-via-color but it only worked in dark theme. Fixed by
   deleting our header entirely (`afca79a`); `writeBlockStart` now
   renders only a dim cwd+branch pill. **Do not** revive the "header
   with concealed echo" pattern unless you've built a full zle
   replacement.

2. **Don't write to xterm from the `?` flow.** `writeRaw` during
   `runAiShortcut` raced with xterm's async parse queue and corrupted
   state by submit time, making commands appear stuck (`a8ab2de`). UI
   feedback during AI wait is a CSS pulse on the editor instead.

3. **Tab always opens completion; `→` accepts ghost.** Matches
   zsh-autosuggestions + tab-completion convention. Don't merge them.

4. **Field order in `LocalLlamaBackend` is load-bearing.** Comments in
   the struct explain; if you need to reorder, verify SIGABRT on
   shutdown doesn't regress.

5. **Model downloads support Range resume.** `cleanup_stranded_parts()`
   preserves `.part` files < 7 days old; the downloader sends
   `Range: bytes=<len>-` and streams into `OpenOptions::append(true)`
   on 206 responses; restarts cleanly on 200. SHA256 rehashes the
   existing .part before streaming continues.

6. **Command specs — 617 commands in `bundle.json`.** Seed content was
   one-time-extracted from withfig/autocomplete TypeScript specs at
   Fig SHA aef52ac; Fig was sunset Sep 2024 and we retired the importer.
   `apps/desktop/src-tauri/completion-specs/bundle.json` (~10 MB) is
   now ArcTerm's own format — `{names[], description, subcommands[],
   options[]}`, recursive — and is maintained by direct JSON edits.
   The original importer script is preserved in the `pre-fig-removal`
   git tag; restore with `git show pre-fig-removal:scripts/import-fig-specs.mjs`
   if you ever want to re-pull from a maintained Fig fork.

7. **Don't use `execCommand("insertText")` in the input editor's paste
   handler.** WebKit's `insertText` silently **strips newline
   characters** from the argument when the caret is in a flat
   contenteditable `<div>`. Symptom that motivated the fix (`b244c78`):
   pasting three separate commands collapsed them into one
   concatenated blob with no separator, which then submitted as a
   single malformed command (`head -1curl …` → "head: illegal line
   count"). The current `onPaste` normalizes `\r\n?` → `\n` and
   inserts a plain text node with literal `\n` characters via the
   Range API; CSS `white-space: pre-wrap` renders them as line breaks
   and `innerText` round-trips them so `getValue()` returns the true
   multi-line string. **Do not** revert to execCommand for paste;
   if you need clipboard-driven edits elsewhere, use the same
   Range-insertion pattern.

---

## Current status & roadmap

### Where we are
Solidly in "feature-complete alpha." Core terminal, multi-session,
AI (Claude + local Gemma), settings panel, completion, history
overlay, theming, native menu, CI + release pipeline, and a
substantial security posture (phase-8 through phase-8.6) all shipped.
Tests green, clippy green, cargo-audit green.

**Not beta yet.** Blockers — see roadmap below.

### Phase 7 tail (folded into Phase 9 below)
- 🔲 **Global search** (`⌘⇧F`). Search across command history +
  session buffers. Reuse history-overlay component pattern.
- 🔲 **Session rename persistence.** Renames work in-memory but die
  on restart. Small — extend `SettingsStore` schema with a
  `sessions: Vec<PersistedSession>` field, save on rename, restore
  on boot.
- 🔲 **`⌘⏎` agent conversation mode.** Streaming chat panel that
  proposes + (with approval) executes commands. Big UX surface —
  explicitly deferred to v1.1 in the current plan, not a beta blocker.

### Outstanding maintenance tasks
- 🔲 **Refresh the command specs in `bundle.json`.** Seed content is
  from Fig SHA `aef52ac` (~Apr 2025); Fig was sunset Sep 2024 so
  some specs are stale (newer `docker compose` flags, recent `kubectl`
  API additions, etc.). Plan: hand-edit the top ~20 commands that
  matter (git, docker, kubectl, npm, cargo, aws, gh, brew, ssh,
  python, node, pnpm, rg, fd, bat, jq, curl, tmux, vim, ssh). Rest
  can stay on the Fig-era import until someone asks. Optional: split
  bundle.json into per-command sidecar files (`git.json`,
  `docker.json`, …) so diffs are readable. Not urgent.
- 🔲 **AI-originated block marker in xterm** — Wave 3 visual polish
  deferred. Paint a distinct pill color on the block-start when the
  command came from an AI "Run" button, so the user can visually
  distinguish AI-originated blocks from their own typing in the
  scrollback. Aesthetic, not security-critical.

### Roadmap to beta

**Phase 9 — close the alpha gaps (~1 week of focused work):**
- Session rename persistence (1-2 hrs)
- Global search `⌘⇧F` (half day)
- Written decision: "VS Code extension is v1.x, not v1.0." Put in
  CONTRIBUTING.md or ROADMAP.md so contributors don't start on it
  expecting it to land soon.
- Explicit defer of `⌘⏎` agent mode to v1.1 (also documented).

**Phase 10 — distribution polish (~2 days):**
- Ad-hoc codesign (`codesign --force --deep --sign - ArcTerm.app`)
  so Gatekeeper's complaint is less hostile than fully-unsigned.
  Does not replace notarization (which is parked per owner's call —
  see "Signing & notarization decision" below) but reduces friction
  for beta users.
- README: prominent "how to install" section with `xattr -dr` path
  clearly documented.
- Homebrew tap (`homebrew-arcterm` repo) so `brew install --cask
  arcterm` works. ~30 min of work; dramatically lowers install
  friction.
- GitHub release notes template for consistent changelogs per tag.

**Phase 11 — public exposure + feedback loop (~3-4 weeks elapsed,
mostly not coding time):**
- Small curated beta tester group; announce on HN / r/commandline /
  an AI-tools community.
- GitHub Discussions enabled; issue labels (`bug`, `enhancement`,
  `question`, `security`).
- 2-week bug-fixing sprints where the priority is "fix what beta
  users reported" — no new features during this window.
- **Explicitly no telemetry.** The terminal audience is hostile to
  phone-home behavior. `/arcterm-audit` + user-submitted bug reports
  are the entire feedback loop.

**Ship criteria for calling `v0.2.0-beta.1`:**
- ✓ Session rename persists across restart
- ✓ Global search works
- ✓ Install via `brew install --cask arcterm` (or equivalently
  low-friction path)
- ✓ ≥ 10 external testers have used it for a week without a P0 bug
- ✓ Explicit written scope: what's v1.0, what's v1.1+
- ✓ CI + release pipeline proven green for 3+ tagged releases

### Signing & notarization decision
Owner's call (Vatsal): **parked indefinitely** while the project
stays free + solo-maintained. $99/yr Apple Developer Program isn't
justified for a free project with no revenue. Distribution strategy
is unsigned `.dmg` + documented `xattr -dr com.apple.quarantine`
workaround + eventual Homebrew tap. Revisit IF the project gains a
sponsor, a company, or a revenue stream. Until then: this is fine,
every open-source Mac project in the world does the same thing.

### Deferred explicitly (not beta blockers)
- **VS Code extension (Phase 6)** → v1.x feature
- **`⌘⏎` agent conversation mode** → v1.1 feature
- **Notarized distribution** → when there's a funding model
- **Regen-from-source CI for bundle.json** → Fig is dead; not useful
- **Linux / Windows ports** → macOS-only alpha was intentional;
  reopen if real demand surfaces post-beta

### After beta
Phase 6 (VS Code extension) — shared TypeScript core in a webview
panel, `node-pty` instead of `portable-pty`, same Claude CLI
subprocess approach. Meaningful rewrite for another environment,
best tackled as its own milestone after the desktop v1.0 is stable.

---

## Gotchas for new sessions

- **Running dev binary has no icon.** `pnpm tauri dev` runs
  `target/debug/arcterm-desktop` directly (no .app bundle) so macOS
  shows a generic icon. Use `pnpm tauri build --debug` to produce a
  proper `.app` that shows the icon. `--debug` also sidesteps the
  deployment-target issue below for quick local iteration.
- **`pnpm tauri build` (release) needs MACOSX_DEPLOYMENT_TARGET set
  locally** on modern Xcode SDKs (15+). llama-cpp-sys-2 pulls in
  `std::filesystem::path` which MacOSX*.sdk marks as introduced in
  10.15; without an explicit target, compilation fails with "`~path`
  is unavailable". CI handles this via workflow-level env; locally
  you need to set it yourself:
  `MACOSX_DEPLOYMENT_TARGET=11.0 CMAKE_OSX_DEPLOYMENT_TARGET=11.0
  pnpm tauri build`. Empirically, `pnpm tauri build --debug` on the
  same machine hasn't hit this — mechanism unclear, but if you just
  need a working `.app` locally, `--debug` is the friction-free path.
  Installing the built
  `.app` into `/Applications` is just
  `rm -rf /Applications/ArcTerm.app && cp -R
  apps/desktop/src-tauri/target/debug/bundle/macos/ArcTerm.app
  /Applications/`.
- **macOS icon cache is sticky.** After a new icon, `killall Dock`
  usually works; stubborn cache needs
  `sudo rm -rf /Library/Caches/com.apple.iconservices.store`.
- **Restart `tauri dev` after Rust changes.** Vite HMR covers frontend
  automatically; Rust doesn't rebuild until you kill + re-run.
- **Two `claude` binaries can coexist** (`~/.local/bin/claude` from
  Anthropic installer + `/opt/homebrew/bin/claude` from the `claude-code`
  brew cask). The brew one is NOT logged in. If `⌘K` 401s silently,
  `which claude` and ensure only the logged-in one is on PATH.
- **Git history was scrubbed once** (commit `0abc4f9` removed local
  brief files from all commits). If you see unexpected orphan commits
  in `git log`, that's why.
- **Never write `Co-Authored-By: Claude` in commits** (Dos_And_Donts rule
  #1). Every commit so far is authored as Vatsal Raicha.

---

## Hard rules (from `Dos_And_Donts.md`)

1. **Never include Claude in any git commit.** No `Co-Authored-By`, no
   "generated with Claude" footers. The word "claude" in commit
   messages is fine when it refers to the CLI we integrate with (e.g.
   `fix(ai/claude):`).
2. **Never delete the `.claude` directory.** It's gitignored at the
   repo root; keep it that way.
3. **Write comments that explain *why*, not *what*.** The codebase is
   dense with "here's the failure mode that motivated this line"
   comments; keep that pattern. Examples in every file but especially
   `ai/local_llama.rs`, `completion/fs.rs`, `models/downloader.rs`.

---

## How to "land" fresh

When you start a new session, do this order:

1. `git log --oneline -5` — see the last few commits; any in-progress
   work surfaces here.
2. `git status` — uncommitted work to continue or revert.
3. `git tag -l` — confirm the last phase tag; any new phase planning
   probably starts here.
4. Skim this file.
5. Check the **Roadmap to beta** section (Phase 9 specifically) — that's
   the current active backlog. Phase 7 tail items are folded in.
   **Outstanding maintenance tasks** above it (refresh specs, AI-block
   marker) are lower priority but valid pickups.
6. For any file the user mentions, start reading from the top — the
   module-level `//!` doc comment tells you what it owns and why.

That should get you from cold-start to contributing within 10 minutes.
