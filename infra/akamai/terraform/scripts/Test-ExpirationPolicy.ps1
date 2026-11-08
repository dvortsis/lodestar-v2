<#
.SYNOPSIS
    Strict expiration tag and lease validator for Lodestar test machines.
.DESCRIPTION
    Enforces format, future validity, minimum lease (15 mins), and maximum ceiling (48 hours).
    Throws terminating errors on invalid, missing, or unreasonable inputs.
#>
function Assert-LodestarExpiration {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExpiresAt,

        [Parameter(Mandatory = $false)]
        [int]$MaxHours = 48,

        [Parameter(Mandatory = $false)]
        [int]$MinMinutes = 15
    )

    if ([string]::IsNullOrWhiteSpace($ExpiresAt)) {
        throw "Expiration tag validation failed: 'ExpiresAt' cannot be null, empty, or whitespace."
    }

    # 1. Strict ISO 8601 UTC format check
    $isoRegex = '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$'
    if ($ExpiresAt -notmatch $isoRegex) {
        throw "Expiration tag validation failed: '$ExpiresAt' does not match strict ISO 8601 UTC format 'yyyy-MM-ddTHH:mm:ssZ'."
    }

    # 2. Parse DateTime as UTC
    $exactFormat = "yyyy-MM-ddTHH:mm:ssZ"
    $culture = [System.Globalization.CultureInfo]::InvariantCulture
    $style = [System.Globalization.DateTimeStyles]::AssumeUniversal -bor [System.Globalization.DateTimeStyles]::AdjustToUniversal
    [DateTime]$parsedDate = [DateTime]::MinValue

    if (-not [DateTime]::TryParseExact($ExpiresAt, $exactFormat, $culture, $style, [ref]$parsedDate)) {
        throw "Expiration tag validation failed: '$ExpiresAt' is not a valid calendar date/time."
    }

    $nowUtc = [DateTime]::UtcNow

    # 3. Future validity check (must be at least MinMinutes in the future)
    $minFuture = $nowUtc.AddMinutes($MinMinutes)
    if ($parsedDate -lt $minFuture) {
        $minsRemaining = ($parsedDate - $nowUtc).TotalMinutes
        throw "Expiration tag validation failed: '$ExpiresAt' must be at least $MinMinutes minutes in the future (remaining: $([math]::Round($minsRemaining, 1))m)."
    }

    # 4. Maximum ceiling check (must not exceed MaxHours in the future)
    $maxFuture = $nowUtc.AddHours($MaxHours)
    if ($parsedDate -gt $maxFuture) {
        $hours = ($parsedDate - $nowUtc).TotalHours
        throw "Expiration tag validation failed: '$ExpiresAt' ($([math]::Round($hours, 2)) hours) exceeds maximum allowable lease of $MaxHours hours."
    }

    return [PSCustomObject]@{
        IsValid   = $true
        ParsedUtc = $parsedDate
        IsoString = $ExpiresAt
        LeaseHours = [math]::Round(($parsedDate - $nowUtc).TotalHours, 3)
    }
}
