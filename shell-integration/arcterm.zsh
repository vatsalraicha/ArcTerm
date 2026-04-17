# ArcTerm zsh integration — sourced after the user's own .zshrc so we can
# override PROMPT and install hooks without clobbering their environment.
#
# Phase 2 responsibilities:
#   1. Suppress the shell's prompt rendering. ArcTerm draws its own prompt
#      inside the custom input editor, so we don't want the shell to paint
#      one too (double prompts, extra blank lines).
#   2. Announce the working directory via OSC 7 whenever it changes so the
#      UI can display an up-to-date cwd.
#
# Later phases (blocks, command metadata, exit codes) add precmd/preexec
# hooks that emit DCS sequences; those live here too when they arrive.

# --- 1. Prompt suppression ----------------------------------------------------
# Empty strings are respected by zsh even when plugins (starship, powerlevel)
# attempt to set their own PROMPT during precmd. To win against late-binding
# plugins, we also set up a precmd hook that re-blanks the prompt every line.
PROMPT=''
RPROMPT=''
PS1=''
RPS1=''

_arcterm_suppress_prompt() {
    # Re-assert blank prompt each command so prompt-replacement plugins
    # (e.g. oh-my-zsh, starship) don't visibly win the race after we set it.
    PROMPT=''
    RPROMPT=''
    PS1=''
    RPS1=''
}

# --- 2. OSC 7 cwd reporting ---------------------------------------------------
# OSC 7 is the de-facto standard for telling terminals "I'm now in this dir"
# (used by macOS Terminal, iTerm, VSCode, GNOME Terminal). Format is:
#     ESC ] 7 ; file://<host>/<path> BEL
# ArcTerm's xterm.js OSC 7 handler decodes this and fires a cwd update.
_arcterm_emit_cwd() {
    # Percent-encode the minimum set of characters a URL parser will choke
    # on. Full RFC 3986 encoding is overkill here — we only need to escape
    # characters that break file://host/path parsing.
    local path="${PWD}"
    path="${path// /%20}"
    path="${path//$'\t'/%09}"
    # HOST is set by zsh automatically; fall back to a literal if somehow not.
    printf '\e]7;file://%s%s\a' "${HOST:-localhost}" "${path}"
}

# Register hooks. add-zsh-hook is idempotent — safe to re-source this file.
autoload -Uz add-zsh-hook
add-zsh-hook precmd _arcterm_suppress_prompt
add-zsh-hook chpwd _arcterm_emit_cwd

# Emit the initial cwd now so ArcTerm's prompt bar is correct from the first
# keystroke, not just after the user cd's somewhere.
_arcterm_emit_cwd
