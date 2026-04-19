# ArcTerm zdotdir .zshenv
#
# When ZDOTDIR is set, zsh loads .zshenv from there instead of $HOME. We
# want the user's own .zshenv (PATH, env exports, etc.) to run first so
# their shell behaves exactly like a normal session.
#
# This file is auto-managed by ArcTerm — any edits will be overwritten on
# next app start.

# SECURITY: capture the per-session OSC nonce into a shell-local variable
# and immediately unset the env var so child processes don't inherit it.
# .zshenv runs BEFORE .zshrc and BEFORE anything user-controlled, so this
# is the earliest point we can scrub the env. arcterm.zsh (sourced later
# from .zshrc) reads $__arcterm_osc_nonce to stamp OSC 133/1337 emissions.
# typeset -g keeps the variable reachable across .zshenv -> .zshrc in
# the same shell but prevents it from being exported to subprocesses.
typeset -g __arcterm_osc_nonce="${ARCTERM_OSC_NONCE-}"
unset ARCTERM_OSC_NONCE

[[ -r "${HOME}/.zshenv" ]] && source "${HOME}/.zshenv"
