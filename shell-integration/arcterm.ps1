# ArcTerm PowerShell integration -- parity with arcterm.zsh.
#
# What this file emits to ArcTerm:
#   OSC 7   file://localhost/C:/path       -- at each prompt, when the location changed
#   OSC 133;C;<nonce> (+ ESC[0m)          -- after PSReadLine accepts a line, before it runs
#   OSC 133;D;<exit>;<nonce>              -- at each prompt, for the command that just finished
#   OSC 1337;ArcTermBranch=<name>;<nonce> -- at each prompt
#
# How it is loaded: NOT dot-sourced. ArcTerm hands this whole file to
# `pwsh -NoLogo -NoExit -EncodedCommand ...`. PowerShell's execution policy
# blocks every .ps1 file on a stock Windows client (`Restricted`) but does
# not apply to commands passed on the command line, so dot-sourcing would
# silently leave the user with no blocks, no cwd and no exit codes.
# Everything here runs in the global scope AFTER the user's $PROFILE
# (PowerShell fixes that order), which is the same contract as zsh: user
# rc first, our hook last, so our prompt wins. Comment-only lines are
# stripped before encoding to keep the command line short, so keep code
# and comments on separate lines.
#
# PowerShell has no preexec / precmd / chpwd hooks. The equivalents used:
#   precmd  -> the `prompt` function (runs before every prompt)
#   preexec -> PSConsoleHostReadLine, the function PSReadLine calls to read
#              a line; 133;C is written right after it returns
#   chpwd   -> compare the location at each prompt with the last one seen

# --- 0. Nonce capture (SECURITY) -----------------------------------------
# Same contract as .zshenv: copy the per-session nonce into a global
# variable and scrub it from the environment so child processes never see
# it. The user's $PROFILE has already run by now (PowerShell's ordering,
# not ours), so anything it spawned did see the variable -- the same narrow
# window fish has; see arcterm.fish.
$global:__arcterm_osc_nonce = [string]$env:ARCTERM_OSC_NONCE
Remove-Item Env:\ARCTERM_OSC_NONCE -ErrorAction SilentlyContinue

$global:__arcterm_esc = [char]27
$global:__arcterm_bel = [char]7
$global:__arcterm_last_cwd = $null
# Resolved once: Get-Command costs tens of milliseconds and the answer does
# not change within a session.
$global:__arcterm_has_git = $null -ne (Get-Command git -CommandType Application -ErrorAction SilentlyContinue)

# --- 1. OSC 7 cwd reporting ----------------------------------------------
function global:__arcterm_emit_cwd {
    $loc = $ExecutionContext.SessionState.Path.CurrentLocation
    # Only filesystem locations map onto a file:// URL. Registry:\ and the
    # other providers are skipped; the UI keeps the last real directory.
    if ($loc.Provider.Name -ne 'FileSystem') { return }
    $p = $loc.ProviderPath
    if ($p -eq $global:__arcterm_last_cwd) { return }
    $global:__arcterm_last_cwd = $p
    # `C:\Users\me` -> `/C:/Users/me`. Forward slashes so the same string
    # works in the completion engine and history on every platform; the
    # leading slash is what a file:// URL puts between host and path, and
    # the frontend strips it again for drive-letter paths. UNC paths
    # (\\server\share) become //server/share and are rejected as
    # non-absolute by the frontend -- acceptable until someone needs them.
    $u = '/' + ($p -replace '\\', '/')
    $u = $u -replace ' ', '%20'
    [Console]::Write("$global:__arcterm_esc]7;file://localhost$u$global:__arcterm_bel")
}

# --- 2. Git branch reporter (custom OSC 1337 key/value) ------------------
function global:__arcterm_emit_branch {
    $branch = ''
    if ($global:__arcterm_has_git) {
        # Same command as arcterm.zsh: empty on detached HEAD, fails (and
        # stays empty) outside a repo. stderr is discarded so a failed git
        # never paints red text at every prompt.
        $branch = [string](& git symbolic-ref --quiet --short HEAD 2>$null)
        if ($LASTEXITCODE -ne 0) { $branch = '' }
    }
    [Console]::Write("$global:__arcterm_esc]1337;ArcTermBranch=$branch;$global:__arcterm_osc_nonce$global:__arcterm_bel")
}

# --- 3. Block-end marker + prompt suppression (precmd) -------------------
function global:prompt {
    # $? and $LASTEXITCODE describe the command that just finished. Read
    # them before anything else in this function can disturb them.
    $ok = $?
    $native = (Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue).Value
    $code = 0
    if (-not $ok) {
        # A failed cmdlet leaves $LASTEXITCODE untouched (only native
        # executables set it), so a cmdlet error after a successful git
        # would otherwise read as 0. Same rule as Windows Terminal's shell
        # integration: the native code when it is non-zero, else 1.
        if ($null -ne $native -and $native -ne 0) { $code = $native } else { $code = 1 }
    }
    [Console]::Write("$global:__arcterm_esc[0m$global:__arcterm_esc]133;D;$code;$global:__arcterm_osc_nonce$global:__arcterm_bel")
    __arcterm_emit_branch
    __arcterm_emit_cwd
    # Our git call above would otherwise overwrite $LASTEXITCODE before the
    # user can read it at the next prompt.
    $global:LASTEXITCODE = $native
    # The visible prompt is ArcTerm's job (the cwd pill above the input).
    # Return something that paints nothing: the string must be non-empty or
    # PowerShell substitutes its default "PS>".
    return "$global:__arcterm_esc[0m"
}

# --- 4. Block-start marker (preexec) -------------------------------------
# PSReadLine reads each line through PSConsoleHostReadLine. By the time
# ReadLine returns, the user's command has been echoed (with syntax
# colours) and the cursor is on a fresh line -- the same moment zsh's
# preexec fires. ArcTerm relies on that echo as the visible command text,
# exactly as it relies on zle's on macOS.
if (Get-Module -Name PSReadLine) {
    function global:PSConsoleHostReadLine {
        $line = [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine($Host.Runspace, $ExecutionContext)
        [Console]::Write("$global:__arcterm_esc]133;C;$global:__arcterm_osc_nonce$global:__arcterm_bel$global:__arcterm_esc[0m")
        $line
    }
}

# Emit initial state so ArcTerm's prompt bar is correct before the first
# prompt is even drawn.
__arcterm_emit_cwd
__arcterm_emit_branch
