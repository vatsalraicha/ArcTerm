# Installing ArcTerm

Three ways to get ArcTerm running, ordered easiest → most involved.

1. [Release download](#release-download-easiest) — signed `.dmg`, drag into Applications.
2. [Build from source, quick](#build-from-source-quick) — for testing a branch.
3. [Full dev setup](#full-dev-setup-contributing) — iterate on the code.

At the end: [optional AI setup](#optional-ai-setup) and [troubleshooting](#troubleshooting).

---

## Release download (easiest)

1. Go to [Releases](https://github.com/vatsalraicha/ArcTerm/releases).
2. Under the latest tag, grab the `.dmg` for your Mac:
   - **Apple Silicon** (M1 / M2 / M3 / M4 / …): `ArcTerm_X.Y.Z_aarch64.dmg`
   - **Intel**: `ArcTerm_X.Y.Z_x64.dmg`
3. Open the `.dmg`, drag **ArcTerm** into your `/Applications` folder.
4. Launch.

**First-launch warning.** Because ArcTerm isn't signed/notarized yet
(waiting for an Apple Developer Program membership at v1.0), macOS will
show *"ArcTerm is from an unidentified developer"* on first open.
Bypass it:

- Right-click (or ⌃-click) `ArcTerm.app` in Finder → **Open** → confirm.
- After you do this once, subsequent launches open normally.

---

## Build from source (quick)

Use this if you want a specific branch or the latest HEAD.

### Prerequisites

| Tool | How to install |
|---|---|
| **Xcode Command Line Tools** | `xcode-select --install` |
| **Rust** (stable) | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| **Node.js** 20+ | `brew install node` |
| **pnpm** | `npm install -g pnpm` (or `brew install pnpm`) |

### Build

```bash
git clone https://github.com/vatsalraicha/ArcTerm.git
cd ArcTerm
pnpm install
cd apps/desktop
pnpm tauri build
```

Wait 8–15 minutes. On success:

```bash
open src-tauri/target/release/bundle/macos/ArcTerm.app
```

Copy it to `/Applications` to install permanently.

For a faster iteration loop use `pnpm tauri build --debug` — produces
the same `.app` with debug symbols (much faster compile, larger binary).

---

## Full dev setup (contributing)

If you're going to edit the code, run the dev server instead of rebuilding
the bundle for every change.

### Prerequisites

Everything in [the previous section](#prerequisites) plus:

```bash
# Optional but very useful: clippy + rustfmt for style (CI enforces both)
rustup component add clippy rustfmt
```

### Run dev

```bash
git clone https://github.com/vatsalraicha/ArcTerm.git
cd ArcTerm
pnpm install

cd apps/desktop
pnpm tauri dev
```

This does three things in parallel:

- **Vite** dev server for the frontend with hot-module reload
- **cargo** builds the Rust side
- **Tauri** launches the app window wired to the Vite server

Frontend edits reload instantly. Rust edits require killing the dev
binary (⌘Q the app) and restarting `pnpm tauri dev` — cargo watch is
NOT automatic in tauri dev.

### Run CI checks locally

Same gates as GitHub Actions:

```bash
# From apps/desktop/src-tauri/
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test                         # includes --doc
cd ../
pnpm vite:build
```

Get these four green before pushing.

### Architecture docs

Start with [`CLAUDE.md`](./CLAUDE.md) at the repo root — it's a dense
~10-minute handbook covering module layout, data flows, and the
non-trivial design decisions. Don't skip it.

---

## Optional AI setup

ArcTerm works out of the box as a plain terminal. To enable the AI
features (⌘K, ⌘⇧E, `?` prefix) you need either the Claude CLI or a
local Gemma model — or both.

### Claude CLI (recommended if you already pay for Claude)

Install and log in with your Pro or Max subscription:

```bash
curl -fsSL https://claude.ai/install.sh | bash
claude /login                      # opens browser, auth against your subscription
```

ArcTerm will detect it at next launch. `⌘K` should just work.

> **Don't set `ANTHROPIC_API_KEY`** in your shell env. ArcTerm strips it
> from the Claude subprocess specifically so subscription auth wins — if
> a stale key is present it can cause 401 auth errors even though your
> subscription is valid.

### Local Gemma 4

Fully offline; no account. Download from inside ArcTerm:

```text
/arcterm-models                     # see what's available
/arcterm-download gemma-4-e2b-it-q4km   # 3.5 GB, good balance
# or
/arcterm-download gemma-4-e4b-it-q8     # 7.5 GB, best quality we ship
```

Download resumes on interruption. When it finishes, the model auto-loads
and backend mode switches to `auto` (Claude preferred, Gemma fallback).

Switch manually at any time:

```text
/arcterm-model claude              # Claude only
/arcterm-model local               # Gemma only
/arcterm-model auto                # Claude → Gemma fallback
/arcterm-load gemma-4-e4b-it-q8    # swap loaded model without changing mode
```

Or open the settings panel with `⌘,`.

### Memory / disk budget

| Variant | Disk | RAM while running |
|---|---|---|
| `gemma-4-e2b-it-iq2m` | 2.6 GB | ~3 GB |
| `gemma-4-e2b-it-q4km` | 3.5 GB | ~4 GB |
| `gemma-4-e4b-it-iq2m` | 4.0 GB | ~5 GB |
| `gemma-4-e4b-it-q4km` | 5.4 GB | ~6.5 GB |
| `gemma-4-e4b-it-q8`  | 8.0 GB | ~9 GB |

On a 16 GB Mac, E4B Q4_K_M is the sweet spot. On 8 GB, stick with E2B
Q4_K_M.

---

## Troubleshooting

### "zsh: command not found: brew" in every new session

Your shell isn't finding Homebrew because `/opt/homebrew/bin` isn't on
PATH. This happens when macOS launches apps from Finder with the minimal
`launchd` PATH.

Fix: already applied in recent builds. ArcTerm spawns your shell with
`-l` (login mode) so `.zprofile` runs before `.zshrc`, setting up brew
and friends. If you still see the error, you're on an older build —
pull the latest and rebuild.

### Claude CLI returns 401 / "Invalid authentication credentials"

Most common cause: you have a stale `ANTHROPIC_API_KEY` in your shell
env AND your Claude Pro/Max session is also valid. ArcTerm strips the
env var from its subprocess, forcing the session path; if the session
itself has expired, you get a 401.

Fix:

```bash
claude /login                      # re-authenticate
```

### Two `claude` binaries installed, wrong one used

If you installed Claude via both the [official script](https://claude.com/claude-code)
AND `brew install claude-code`, you'll have two binaries and only one is
logged in. ArcTerm resolves via PATH, so whichever is first wins.

Fix: pick one.

```bash
# Most people want the official script's install (~/.local/bin/claude)
# and don't need the brew cask.
brew uninstall --cask claude-code
```

### App icon shows a generic/gear icon in the Dock

Only happens during `pnpm tauri dev`. The dev binary is a raw
executable; the `.icns` only applies to bundled `.app`s. Build with
`pnpm tauri build --debug` and open the resulting `.app` to see the
real icon.

### Old icon sticks around after replacing the bundle

macOS caches icons aggressively. Force refresh:

```bash
sudo rm -rf /Library/Caches/com.apple.iconservices.store
rm -rf ~/Library/Caches/com.apple.iconservices.*
killall Dock
killall Finder
```

### Local Gemma download interrupted, won't resume

ArcTerm keeps `.part` files under `~/.arcterm/models/` for up to 7 days
and resumes via HTTP Range requests. If the server rejects Range or the
file is older, the next download starts from zero. You can also
manually clean:

```bash
rm ~/.arcterm/models/*.part
```

### "No local model loaded" when switching to Local mode

Happens if you have GGUFs on disk but boot started with `mode=claude`,
so ArcTerm didn't eagerly load any model. Newer builds lazy-load when
you switch modes — if you're on one, just try again. If not, use
`/arcterm-load <id>` to explicitly load a variant, or `⌘,` to pick one
from the settings panel.

### Model loads but inference output is garbled or echoes your prompt

That's a quantization floor. E2B at IQ2_M (aggressive 2-bit) often
pattern-matches weirdly and echoes the prompt. Use Q4_K_M or step up
to E4B:

```text
/arcterm-download gemma-4-e4b-it-q4km
/arcterm-load gemma-4-e4b-it-q4km
```

### Shell prompt looks double / the command appears twice

Shouldn't happen on current builds (the architecture was rewritten to
avoid this — we let zsh's line editor echo the command instead of
drawing our own, eliminating the duplicate at its source). If you see
it, you're on a very old build.

### Any crash on quit (SIGABRT)

Should be fixed for good, but if you see it: confirm you're on latest
main. The fix was field-ordering in `LocalLlamaBackend` so `llama.cpp`
drops its components in the right order — older builds regress on this
if the struct layout changes.

---

## Where things live on disk

| Path | What |
|---|---|
| `/Applications/ArcTerm.app` | Installed app (macOS convention) |
| `~/.arcterm/config.json` | Your settings |
| `~/.arcterm/history.db` | Command history (SQLite) |
| `~/.arcterm/models/` | Downloaded GGUFs |
| `~/.arcterm/shell-integration/` | Auto-managed zsh/bash/fish hook scripts |
| `~/.arcterm/zdotdir/` | zsh ZDOTDIR chain-load files |

Nothing outside `~/.arcterm/` and `/Applications/ArcTerm.app` is created
or modified. Uninstall is just `rm -rf ~/.arcterm && rm -rf /Applications/ArcTerm.app`.

---

## Uninstall

```bash
# Settings + models + history (all app state)
rm -rf ~/.arcterm

# The app
rm -rf /Applications/ArcTerm.app

# Claude Code auth + cache (optional — only if you're done with claude CLI too)
# rm -rf ~/.claude
```
