<#
.SYNOPSIS
    Typed-confirmation guard shared by the paid and destructive scripts.
    Dot-source it: . (Join-Path $PSScriptRoot 'Interactive.ps1')
.DESCRIPTION
    The confirmation phrase must be typed by a person at a console. A phrase piped
    in (`'DEPLOY 2 LINODES' | .\Deploy.ps1`), read from a redirected stdin, or sent
    by an automated, non-interactive session is refused before Read-Host runs.
#>

function Assert-InteractiveConfirmation {
    $redirected = $false
    try { $redirected = [Console]::IsInputRedirected } catch { $redirected = $true }
    $nonInteractiveArg = [Environment]::GetCommandLineArgs() | Where-Object { $_ -match '^-NonI(nteractive)?$' }
    if ($redirected -or -not [Environment]::UserInteractive -or $nonInteractiveArg) {
        throw 'Refusing to read the confirmation phrase: input is redirected or the session is not interactive. A person must type it at a console. Nothing was changed.'
    }
}
