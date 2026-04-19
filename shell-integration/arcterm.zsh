# ArcTerm zsh integration — sourced after the user's own .zshrc so we can
# override PROMPT and install hooks without clobbering their environment.
#
# What this file emits to ArcTerm:
#   OSC 7   file://host/cwd       — on every directory change (Phase 2)
#   OSC 133;D;<exit>               — after each command finishes (Phase 3)
#   OSC 1337;ArcTermBranch=<name>  — after each command (Phase 3, custom)
#
# Together these let the UI place visual block boundaries, store exit
# codes in history, and show the git branch in the prompt bar.

# --- 1. Prompt suppression ---------------------------------------------------
# Empty strings are respected by zsh even when plugins (starship, powerlevel)
# attempt to set their own PROMPT during precmd. We also install a precmd
# hook to re-blank on every command so late-binding plugins don't win the
# race after this file runs once.
PROMPT=''
RPROMPT=''
PS1=''
RPS1=''

_arcterm_suppress_prompt() {
    PROMPT=''
    RPROMPT=''
    PS1=''
    RPS1=''
}

# --- 2. OSC 7 cwd reporting --------------------------------------------------
# OSC 7 is the de-facto standard "I'm in this directory now" signal used by
# macOS Terminal, iTerm, VSCode, GNOME Terminal, etc. Format:
#     ESC ] 7 ; file://<host>/<path> BEL
# ArcTerm's xterm.js OSC 7 handler decodes this and updates the prompt bar.
_arcterm_emit_cwd() {
    # Minimal URL-safe encoding: only the characters a file-URI parser
    # tends to choke on. Full RFC 3986 escaping is overkill for display.
    local path="${PWD}"
    path="${path// /%20}"
    path="${path//$'\t'/%09}"
    printf '\e]7;file://%s%s\a' "${HOST:-localhost}" "${path}"
}

# --- 3. Block-start / block-end markers -------------------------------------
# OSC 133 is the FinalTerm / iTerm shell-integration contract used by
# Warp, WezTerm, kitty, iTerm2 and VSCode.
#
# We emit two markers:
#
#   ;C  — fired from preexec, right after zle has echoed the user's command
#         back to the terminal but before the command actually runs. ArcTerm
#         uses this as the moment to un-conceal: writeBlockStart paints the
#         styled "❯ <command>" header and then sets the foreground color to
#         match the background, making zle's duplicate echo invisible. The
#         \e[0m emitted here undoes that conceal so the real command output
#         renders normally.
#
#   ;D;<exit> — fired from precmd after each command finishes. Payload is
#         the exit status; ArcTerm uses it to close the block and update
#         the history row.
#
# The \e[0m prefix on ;D is belt + suspenders in case preexec was skipped
# (e.g. the user pressed Ctrl+C between zle read and preexec firing).
# SECURITY: OSC 133 emissions include the per-session nonce (captured in
# .zshenv from $ARCTERM_OSC_NONCE before any user code runs). ArcTerm's
# frontend only acts on OSC 133 sequences whose nonce matches the session's
# stored value, so a rogue `cat file-containing-crafted-bytes` or remote
# ssh server output can't spoof "command finished with exit 0" and
# corrupt the history DB. Wire format (optional-nonce is semicolon-delimited
# so older ArcTerm builds that ignore the trailing field still parse exit):
#   \e]133;C;<nonce>\a   (preexec — command about to run)
#   \e]133;D;<exit>;<nonce>\a  (precmd — command finished)
_arcterm_mark_command_executed() {
    printf '\e]133;C;%s\a\e[0m' "${__arcterm_osc_nonce-}"
}

_arcterm_emit_block_end() {
    local exit_code=$?
    printf '\e[0m\e]133;D;%d;%s\a' "${exit_code}" "${__arcterm_osc_nonce-}"
    return ${exit_code}
}

# --- 4. Git branch reporter (custom OSC 1337 key/value) ----------------------
# OSC 1337 is iTerm's private-use code; we namespace our key with the
# ArcTerm prefix so it never collides with iTerm's own keys. Empty value
# when cwd isn't a git repo — the UI hides the branch label in that case.
_arcterm_emit_branch() {
    local branch=""
    # --show-current is fast and git-internal (no fork of `sed`). It prints
    # empty string when HEAD is detached; stderr suppressed for the
    # "not a git repo" case.
    branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || branch=""
    # SECURITY: nonce-stamped so spoofed ArcTermBranch values from
    # crafted program output are rejected by the frontend validator.
    # Wire format: `ArcTermBranch=<name>;<nonce>` — git ref names can't
    # contain `;` (see git-check-ref-format) so the split is unambiguous.
    printf '\e]1337;ArcTermBranch=%s;%s\a' "${branch}" "${__arcterm_osc_nonce-}"
}

# --- 5. Register hooks -------------------------------------------------------
autoload -Uz add-zsh-hook

# precmd runs right before each new prompt (i.e. after a command finishes,
# or at the very start of the shell). Order matters: block-end captures $?
# before any other hook could stomp on it, so run it first.
#
# preexec runs AFTER zle reads the line (and echoes it to the terminal) but
# BEFORE the command executes. It's the right moment to un-conceal zle's
# echo so the actual command output renders normally.
add-zsh-hook precmd  _arcterm_emit_block_end
add-zsh-hook precmd  _arcterm_suppress_prompt
add-zsh-hook precmd  _arcterm_emit_branch
add-zsh-hook preexec _arcterm_mark_command_executed
add-zsh-hook chpwd   _arcterm_emit_cwd

# Emit initial state so ArcTerm's prompt bar is correct from the first
# keystroke, not just after the user cd's somewhere.
_arcterm_emit_cwd
_arcterm_emit_branch
