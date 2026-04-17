# ArcTerm zdotdir .zlogin
#
# Read LAST by zsh for login shells (after .zprofile + .zshrc). Rare
# that users put anything here, but we chain to the user's own just
# in case — skipping it would be a silent "why doesn't my fancy
# login hook run" for anyone who does use .zlogin.
#
# This file is auto-managed by ArcTerm — edits will be overwritten
# on next app start.

[[ -r "${HOME}/.zlogin" ]] && source "${HOME}/.zlogin"
