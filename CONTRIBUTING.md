# Contributing to ArcTerm

Thanks for taking the time. ArcTerm is early — v0.1.0 alpha — and the
code moves fast, but thoughtful issues and pull requests are very welcome.
This file is the shortest path from "I noticed something" to "it landed
in main".

## Project scope (please read first)

Knowing what's **in** and **out** of scope saves everyone time.

**In scope**
- Bug fixes on the current platform target: **macOS 11+**, Apple Silicon
  and Intel.
- Phase 7 polish items listed in [`CLAUDE.md`](./CLAUDE.md) under
  *Phase 7 status & what's left* — most notably global search (`⌘⇧F`),
  persistent session rename, and `⌘⏎` agent conversation mode.
- Quality-of-life fixes: keybinding issues, AI prompt tweaks, completion
  spec patches, shell-integration robustness.

**Out of scope (for now)**
- **Linux or Windows support.** Explicitly deferred; touching
  cross-platform boilerplate right now creates maintenance we can't
  afford at this stage. A Linux/Windows port is a separate conversation
  post-1.0.
- **VS Code extension** (Phase 6). Planned but not started; please don't
  open PRs for it yet.
- **Large architectural rewrites** without a prior discussion. ArcTerm
  has load-bearing quirks (PTY echo handling, field drop order in
  `LocalLlamaBackend`, block-rendering protocol, etc.) that aren't
  obvious from the code alone. Open an issue first and let's align
  before you spend a weekend on it.

## Reporting bugs

Use the **Bug report** issue template. It asks for:

- ArcTerm version (`About ArcTerm` in the app menu, or the release tag
  you downloaded)
- macOS version + chip (Apple Silicon / Intel)
- Minimal reproduction steps
- What you expected vs. what happened

Before filing, a 30-second check: does the bug also reproduce in another
terminal emulator (Terminal.app, iTerm2)? If yes, it's probably a shell
or config issue upstream of ArcTerm; worth noting in the report either
way, but that framing helps triage faster.

## Suggesting features

Use the **Feature request** template. Describe the **problem** first,
then your proposed solution. "I can't see my last command's exit code at
a glance" is a far more useful opener than "add a status bar" — it lets
the discussion consider alternatives.

If a proposal is large, start an [Issue] or [Discussion] to align before
writing code. Closed or rejected PRs are nobody's favorite outcome.

## Asking questions

For usage or configuration questions, please use [Discussions] rather
than Issues — it keeps the issue tracker focused on tracked work. If
Discussions aren't enabled on the repo yet, a question-tagged issue is
fine.

## Security disclosures

Do **not** file public issues for security vulnerabilities. Open a
private security advisory at
<https://github.com/vatsalraicha/ArcTerm/security/advisories/new> and
include a repro + impact assessment.

## Building from source

```bash
# 1. Dependencies
#    - Rust (stable, 1.77+)
#    - pnpm 10+
#    - Xcode Command Line Tools

# 2. Clone and install
git clone https://github.com/vatsalraicha/ArcTerm.git
cd ArcTerm
pnpm install

# 3. Development loop (Vite HMR + cargo watch)
cd apps/desktop
pnpm tauri dev

# 4. Release bundle (produces .app + .dmg under
#    apps/desktop/src-tauri/target/{debug,release}/bundle/)
pnpm tauri build --debug     # fast, debug profile, proper .app bundle
pnpm tauri build             # release profile; may need
                             # MACOSX_DEPLOYMENT_TARGET=11.0 and
                             # CMAKE_OSX_DEPLOYMENT_TARGET=11.0 on
                             # newer Xcode SDKs for llama-cpp-sys-2
```

Rust changes don't hot-reload — restart `tauri dev` after editing Rust.
Vite handles frontend HMR automatically.

## Before opening a pull request

Please run all four locally. CI enforces each one; failing any is a
wasted round-trip:

```bash
cd apps/desktop/src-tauri
cargo check
cargo clippy -- -D warnings
cargo test

cd ../                         # apps/desktop
pnpm vite:build
```

## Code style

- **Read [`CLAUDE.md`](./CLAUDE.md) first.** It's ~500 lines and gets
  you from zero to oriented in 10 minutes. It also documents the
  non-obvious design decisions ("drop our own command echo", "don't
  write to xterm from the `?` flow", etc.) that will save you from
  reinventing footguns we've already disarmed.
- **Comments explain *why*, not *what*.** Most files have a module-level
  doc comment describing the component's role + load-bearing
  invariants. Follow the existing pattern — especially in
  `ai/local_llama.rs`, `completion/fs.rs`, `models/downloader.rs`.
- **No new dependencies without a line of justification** in the PR
  description. The binary is ~65 MB today and we'd like it to stay
  roughly there.
- **TypeScript**: no frameworks, no build-time magic beyond Vite. Plain
  DOM + modules.
- **Rust**: stable, 1.77+. Prefer `Arc<dyn Trait>` over generics across
  the AI layer so backends can swap at runtime.

## Commit and PR conventions

- **Conventional Commits** for messages: `feat(area):`, `fix(area):`,
  `docs:`, `refactor:`, `chore:`, `test:`. Area examples:
  `input-editor`, `ai/claude`, `pty`, `shell`, `ci`.
- **One logical change per commit.** It's easier to review five small
  focused commits than one 400-line blob.
- **Write the *why* in the commit body.** The first line says what
  changed; the body explains the motivation and any tradeoffs. See
  `git log` for examples.
- **Don't amend after review has started.** Stack a new commit so
  reviewers can see what you changed in response to feedback.
- **Rebase onto main before merging** so history stays linear.

## Code of Conduct

By participating in this project, you agree to abide by the
[Code of Conduct](./CODE_OF_CONDUCT.md). Reports of abusive behavior can
be filed via a private GitHub security advisory (link above).

## License of contributions

ArcTerm is licensed under [Apache 2.0](./LICENSE). By submitting a pull
request, you agree that your contribution is licensed under the same
terms — Apache 2.0 covers this automatically via §5 (Submission of
Contributions) unless you explicitly state otherwise. No separate CLA is
required.

Brand assets (the "ArcTerm" name and app icons) are covered separately by
[CC BY 4.0](./LICENSE-BRAND.md).

---

Thank you for helping make ArcTerm better.

[Issue]: https://github.com/vatsalraicha/ArcTerm/issues/new/choose
[Discussion]: https://github.com/vatsalraicha/ArcTerm/discussions
[Discussions]: https://github.com/vatsalraicha/ArcTerm/discussions
