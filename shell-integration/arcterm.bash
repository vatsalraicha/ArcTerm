# ArcTerm bash integration — parity with arcterm.zsh.
#
# What this file emits to ArcTerm:
#   OSC 7   file://host/cwd       — whenever PWD changes
#   OSC 133;C (+ \e[0m)            — before each command runs
#   OSC 133;D;<exit>               — after each command finishes
#   OSC 1337;ArcTermBranch=<name>  — after each command (custom key)
#
# bash doesn't have native preexec/precmd hooks the way zsh does, so we
# rely on the widely-used `bash-preexec` pattern: DEBUG trap + PROMPT_COMMAND.
# We inline a minimal implementation rather than require the bash-preexec
# project as a separate dep — the feature set we need is small.
#
# Prompt suppression: setting PS1/PS2 to empty strings. bash re-reads
# these every prompt draw, so we apply at each precmd to win against
# anything the user's rc files set.

# --- 1. Minimal preexec / precmd plumbing -----------------------------------
# `__arcterm_preexec_running` guards against DEBUG trap firing on commands
# invoked inside our own hook code (otherwise infinite recursion).
__arcterm_preexec_running=0
__arcterm_preexec_hook() {
    # BASH_COMMAND holds the command about to run. Skip if we're inside
    # our own hook, inside PROMPT_COMMAND, or at the very first prompt.
    if [[ ${__arcterm_preexec_running} -ne 0 ]]; then return; fi
    # PROMPT_COMMAND runs as a command too; skip those invocations.
    if [[ ${BASH_COMMAND} == ${PROMPT_COMMAND-} ]]; then return; fi
    # Skip if the shell is about to print its (empty) prompt.
    if [[ ${BASH_COMMAND} == __arcterm_precmd_hook ]]; then return; fi
    __arcterm_preexec_running=1
    _arcterm_mark_command_executed
    __arcterm_preexec_running=0
}

__arcterm_precmd_hook() {
    local exit_code=$?
    _arcterm_emit_block_end "${exit_code}"
    _arcterm_suppress_prompt
    _arcterm_emit_branch
    # Fire chpwd if PWD changed since the last prompt. bash has no native
    # chpwd, so we diff against a cached copy.
    if [[ "${PWD}" != "${__arcterm_last_pwd-}" ]]; then
        __arcterm_last_pwd="${PWD}"
        _arcterm_emit_cwd
    fi
    return ${exit_code}
}

# --- 2. Emit helpers (same wire format as the zsh version) -----------------
_arcterm_suppress_prompt() {
    PS1=''
    PS2=''
}

_arcterm_emit_cwd() {
    local path="${PWD}"
    path="${path// /%20}"
    printf '\e]7;file://%s%s\a' "${HOSTNAME:-localhost}" "${path}"
}

# SECURITY: see arcterm.zsh for the OSC nonce threat model + wire format.
# The nonce is captured into $__arcterm_osc_nonce by the bash rcfile
# (generated in shell_hooks.rs::bash_rcfile_contents) before user .bashrc
# sources, and the env var is unset there so child processes don't see it.
_arcterm_mark_command_executed() {
    printf '\e]133;C;%s\a\e[0m' "${__arcterm_osc_nonce-}"
}

_arcterm_emit_block_end() {
    local exit_code="${1:-0}"
    printf '\e[0m\e]133;D;%d;%s\a' "${exit_code}" "${__arcterm_osc_nonce-}"
}

_arcterm_emit_branch() {
    local branch=""
    branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || branch=""
    printf '\e]1337;ArcTermBranch=%s;%s\a' "${branch}" "${__arcterm_osc_nonce-}"
}

# --- 3. Install hooks ------------------------------------------------------
trap '__arcterm_preexec_hook' DEBUG
# Chain our precmd hook onto any existing PROMPT_COMMAND rather than
# clobbering — users commonly extend PROMPT_COMMAND in their rc files.
if [[ -z "${PROMPT_COMMAND-}" ]]; then
    PROMPT_COMMAND='__arcterm_precmd_hook'
else
    # Only add ourselves once even if this file is sourced twice.
    case ";${PROMPT_COMMAND};" in
        *";__arcterm_precmd_hook;"*) ;;
        *) PROMPT_COMMAND="${PROMPT_COMMAND};__arcterm_precmd_hook" ;;
    esac
fi

# Emit initial state so ArcTerm's prompt bar is correct from the first
# keystroke.
__arcterm_last_pwd="${PWD}"
_arcterm_suppress_prompt
_arcterm_emit_cwd
_arcterm_emit_branch
