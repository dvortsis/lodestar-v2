<#
.SYNOPSIS
    Typed-confirmation guard shared by the paid and destructive scripts.
    Dot-source it: . (Join-Path $PSScriptRoot 'Interactive.ps1')
.DESCRIPTION
    The confirmation phrase must be typed by a person at a console. A phrase piped
    to a new process (`'DEPLOY 2 LINODES' | pwsh -File .\Deploy.ps1`), read from a
    redirected stdin, or sent by a non-interactive session is refused before
    Read-Host runs. PowerShell ISE has no console and is refused too.
#>

function Assert-InteractiveConfirmation {
    $redirected = $false
    try { $redirected = [Console]::IsInputRedirected } catch { $redirected = $true }
    # Any accepted spelling of -NonInteractive: -noni, -nonint, ..., /NonInteractive, --noninteractive.
    $nonInteractiveArg = [Environment]::GetCommandLineArgs() | Where-Object {
        $name = $_.TrimStart('-', '/').ToLowerInvariant()
        $_ -match '^(--?|/)noni' -and 'noninteractive'.StartsWith($name)
    }
    if ($redirected -or -not [Environment]::UserInteractive -or $nonInteractiveArg) {
        throw 'Refusing to read the confirmation phrase: input is redirected or the session is not interactive. A person must type it at a console (PowerShell ISE is not supported; use a console or Windows Terminal). Nothing was changed.'
    }
}
