# ArcTerm zdotdir .zshrc
#
# Load order:
#   1. User's $HOME/.zshrc — full untouched environment (aliases, plugins,
#      prompt frameworks like starship/p10k, completion, history, etc).
#   2. ArcTerm's arcterm.zsh — overrides PROMPT and registers OSC 7 hooks.
#      Must come AFTER the user's rc so our PROMPT='' wins the last word.
#
# This file is auto-managed by ArcTerm — any edits will be overwritten on
# next app start. If you want to customize ArcTerm's behavior, edit your
# own ~/.zshrc; we always source it first.

[[ -r "${HOME}/.zshrc" ]] && source "${HOME}/.zshrc"

# ARCTERM_INTEGRATION_DIR is exported by the ArcTerm app when spawning
# the PTY. Fall back to the conventional path so sourcing by hand still
# works during development.
: "${ARCTERM_INTEGRATION_DIR:=${HOME}/.arcterm/shell-integration}"
if [[ -r "${ARCTERM_INTEGRATION_DIR}/arcterm.zsh" ]]; then
    source "${ARCTERM_INTEGRATION_DIR}/arcterm.zsh"
fi
