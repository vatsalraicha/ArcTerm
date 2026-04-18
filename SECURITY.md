# Security policy

Thanks for taking the time to report responsibly. ArcTerm runs arbitrary
shell commands, talks to AI backends over subprocess and network, and
downloads model weights — there's a real security surface, and we want
to hear about issues before they become public problems.

## Supported versions

ArcTerm is in alpha and moves fast. Security fixes land on `main` and
are shipped in the next release tag. **Only the latest release tag is
supported** for security fixes — if you're on an older alpha, the fix
is "upgrade to the latest."

| Version                    | Supported          |
| -------------------------- | ------------------ |
| Latest alpha release tag   | :white_check_mark: |
| Earlier alpha release tags | :x:                |
| `main` branch              | :white_check_mark: |

## Reporting a vulnerability

**Please do not file a public issue.** Public issues are indexed
immediately and tip off attackers before a fix is available.

Instead, open a private **GitHub Security Advisory**:

<https://github.com/vatsalraicha/ArcTerm/security/advisories/new>

That channel is private between you and the maintainers, lets us
collaborate on a fix in a private fork, and supports coordinated
disclosure + CVE assignment when appropriate.

### What to include

A good report lets us reproduce the issue in minutes rather than hours.
Please include as much of the following as you can:

- **Summary** — one sentence describing the vulnerability.
- **Impact** — what an attacker can do, and under what conditions.
  ("Local attacker with access to $HOME can…" vs. "any user visiting
  a malicious URL can…" are very different.)
- **Affected versions** — release tag(s) or commit SHA(s) where the
  issue reproduces.
- **Reproduction steps** — minimal, deterministic, with any required
  setup (config, files, network conditions).
- **Proof of concept** — code, commands, or a short video. Strongly
  preferred over prose descriptions.
- **Suggested fix**, if you have one. Not required.

### What's in scope

- Command-injection or unsandboxed code execution via ArcTerm-specific
  input paths (slash commands, AI responses, completion specs, shell
  integration scripts).
- Tampering with downloaded model files (e.g. SHA verification bypass,
  TLS issues in the downloader).
- Credential exposure — unintended leakage of Anthropic API keys, OAuth
  tokens, or any secret the user has configured.
- Local privilege escalation via the app bundle, Tauri IPC surface, or
  shell-hook install.
- Memory-safety bugs in Rust code that's reachable from untrusted input.

### What's out of scope

- Anything the user explicitly runs in the terminal. ArcTerm is a
  terminal — executing untrusted commands the user typed is the
  feature, not a vulnerability.
- Vulnerabilities in upstream dependencies (`tauri`, `llama-cpp-2`,
  `xterm.js`, etc.) that aren't reachable through ArcTerm's specific
  usage. Please report those upstream; cross-link here if ArcTerm's
  configuration makes the exposure materially worse.
- Missing hardening unrelated to an exploitable issue (e.g. "you
  should enable feature X" with no demonstrated impact).
- Social engineering of the maintainer, bugs in GitHub itself, or
  typosquatted dependencies that aren't actually installed.
- Denial of service that requires the user to already have local
  access and type commands at their own terminal.

## What to expect after reporting

ArcTerm is maintained by a single person in spare time, so please set
expectations accordingly:

- **Initial acknowledgment**: within ~7 days.
- **Triage + severity assessment**: within ~14 days.
- **Fix timeline**: depends on severity and complexity. Critical issues
  get priority and a targeted patch release; lower-severity issues
  roll into the next scheduled release.
- **Disclosure**: once a fix ships, the advisory is published. You'll
  be credited in the advisory unless you ask to remain anonymous.

If you haven't heard back within a week, a gentle nudge on the
advisory is welcome — we may have missed the notification.

## Safe harbor

We consider good-faith security research and responsible disclosure to
be a service to the project. If you:

- Make a reasonable, good-faith effort to avoid privacy violations,
  data destruction, and service disruption,
- Only interact with accounts and data you own or have explicit
  permission to test against,
- Give us reasonable time to remediate before any public disclosure,

…we will not pursue or support legal action against you for your
research, and we'll work with you on coordinated disclosure.

Thanks for helping keep ArcTerm and its users safe.
