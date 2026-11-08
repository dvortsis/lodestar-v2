<#
.SYNOPSIS
    Shared AWS CLI helpers for the Lodestar scripts. Dot-source it: . (Join-Path $PSScriptRoot 'AwsCli.ps1')
.DESCRIPTION
    Windows PowerShell 5.1 and PowerShell 7 treat a native command's stderr differently:
    under $ErrorActionPreference = 'Stop', 5.1 throws on the first stderr line of `2>&1`
    (even when the command succeeds), and 7 mixes ErrorRecords into the output. Invoke-AwsCli
    runs the CLI with 'Continue', keeps stdout and stderr apart, and returns the exit code,
    so callers decide on $result.ExitCode alone and parse only $result.Out.
#>

function Resolve-AwsCli {
    $default = 'C:\Program Files\Amazon\AWSCLIV2\aws.exe'
    if (Test-Path -LiteralPath $default) { return $default }
    $cmd = Get-Command aws -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw 'AWS CLI v2 executable not found.'
}

function Invoke-AwsCli {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $all = & $script:AwsCli @Arguments 2>&1
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $prev
    }
    $out = @($all | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] } | ForEach-Object { [string]$_ })
    $err = @($all | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] } | ForEach-Object { $_.ToString() })
    [PSCustomObject]@{
        ExitCode = $code
        Out      = ($out -join "`n")
        Err      = ($err -join "`n")
    }
}

# Text output gives one tab-separated line per row on every PowerShell version.
# (ConvertFrom-Json unrolls a one-element array differently in 5.1 and 7.)
function ConvertFrom-AwsTextRows([string]$Text) {
    $rows = [System.Collections.Generic.List[string[]]]::new()
    foreach ($line in ($Text -split "`r?`n")) {
        $t = $line.Trim()
        if ($t -and $t -cne 'None') { $rows.Add([string[]]($t -split '\s+')) }
    }
    return , $rows
}

$script:AwsCli = Resolve-AwsCli
