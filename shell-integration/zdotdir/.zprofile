# ArcTerm zdotdir .zprofile
#
# Read by zsh for LOGIN shells. We spawn zsh with `-l` so that PATH-
# related setup (notably Homebrew's `eval "$(brew shellenv)"` line on
# macOS) runs before .zshrc — which is where .zshrc can then find
# `brew` on PATH and source plugins from its prefix.
#
# Without this chain, a ZDOTDIR-launched login shell would look for
# .zprofile inside our zdotdir (empty) and silently skip the user's
# own one. Symptom: `.zshrc:147: command not found: brew` when the app
# is launched from Finder (where the app inherits launchd's minimal
# PATH instead of a terminal's full PATH).
#
# This file is auto-managed by ArcTerm — edits will be overwritten
# on next app start.

[[ -r "${HOME}/.zprofile" ]] && source "${HOME}/.zprofile"
