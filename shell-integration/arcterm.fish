# ArcTerm fish integration — parity with arcterm.zsh.
#
# What this file emits to ArcTerm:
#   OSC 7   file://host/cwd       — whenever PWD changes (fish_cwd_did_change)
#   OSC 133;C (+ \e[0m)            — before each command runs (fish_preexec)
#   OSC 133;D;<exit>               — after each command finishes (fish_postexec)
#   OSC 1337;ArcTermBranch=<name>  — after each command (custom key)
#
# fish has proper preexec/postexec events so the plumbing is the
# cleanest of the three shells. It also has `fish_prompt` as a function
# we can redefine to empty (bash/zsh require flipping a variable).

# --- 1. Prompt suppression -------------------------------------------------
# Override the prompt function. If the user had a custom fish_prompt, we
# replace it for this session — they can still re-run their own setup in
# a subshell. Same compromise we make in zsh/bash.
function fish_prompt
end
function fish_right_prompt
end

# --- 1b. OSC nonce capture (SECURITY) --------------------------------------
# Fish's startup differs from bash/zsh: config.fish always runs first, then
# our -C hooks. That means the $ARCTERM_OSC_NONCE env var is visible to
# anything config.fish spawns — a minor window narrower than bash/zsh's
# unset-before-rc approach. Documented trade-off; the majority target is
# zsh on macOS where the earlier unset works cleanly.
#
# Capture into a global (non-exported) variable and scrub the env var so
# downstream commands (plugins, functions defined after this file sources)
# don't leak the nonce to subprocesses.
set -g __arcterm_osc_nonce $ARCTERM_OSC_NONCE
set -e ARCTERM_OSC_NONCE

# --- 2. Emit helpers -------------------------------------------------------
function _arcterm_emit_cwd
    # fish has built-in %-encoding via `string escape --style=url` but
    # it's more aggressive than we need; stick to the minimal set.
    set -l path (string replace -a ' ' '%20' -- $PWD)
    printf '\e]7;file://%s%s\a' (hostname) $path
end

# SECURITY: OSC 133/1337 emissions stamped with per-session nonce. See
# arcterm.zsh for full threat model + wire format.
function _arcterm_mark_command_executed
    printf '\e]133;C;%s\a\e[0m' $__arcterm_osc_nonce
end

function _arcterm_emit_block_end
    printf '\e[0m\e]133;D;%d;%s\a' $argv[1] $__arcterm_osc_nonce
end

function _arcterm_emit_branch
    set -l branch ""
    set branch (git symbolic-ref --quiet --short HEAD 2>/dev/null; or echo "")
    printf '\e]1337;ArcTermBranch=%s;%s\a' $branch $__arcterm_osc_nonce
end

# --- 3. Event hooks --------------------------------------------------------
# fish 3+ natively supports --on-event for preexec/postexec + the
# PWD-change signal. All three are idiomatic; no DEBUG trap gymnastics.
function _arcterm_preexec --on-event fish_preexec
    _arcterm_mark_command_executed
end

function _arcterm_postexec --on-event fish_postexec
    # fish sets $status (not $?) and it's live immediately post-command.
    _arcterm_emit_block_end $status
    _arcterm_emit_branch
end

function _arcterm_chpwd --on-variable PWD
    _arcterm_emit_cwd
end

# --- 4. Initial emits so prompt bar is correct on first keystroke ----------
_arcterm_emit_cwd
_arcterm_emit_branch
