# ArcTerm zdotdir .zshenv
#
# When ZDOTDIR is set, zsh loads .zshenv from there instead of $HOME. We
# want the user's own .zshenv (PATH, env exports, etc.) to run first so
# their shell behaves exactly like a normal session.
#
# This file is auto-managed by ArcTerm — any edits will be overwritten on
# next app start.

[[ -r "${HOME}/.zshenv" ]] && source "${HOME}/.zshenv"
