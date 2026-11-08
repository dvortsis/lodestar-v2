# Get-McpCredentials.ps1
# Fail-fast credential process wrapper for AWS MCP with MFA caching.
param(
    [string]$RoleName = "LodestarObserver",
    [string]$Profile = "lodestar-observer"
)

$cacheDir = Join-Path $HOME ".aws\cli\cache"
$valid = $false
$now = [DateTime]::UtcNow

if (Test-Path $cacheDir) {
    $cacheFiles = Get-ChildItem -Path $cacheDir -Filter "*.json" -File
    foreach ($file in $cacheFiles) {
        try {
            $data = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
            if ($data.AssumedRoleUser.Arn -match $RoleName) {
                $exp = [DateTime]::Parse($data.Credentials.Expiration).ToUniversalTime()
                if ($exp -gt $now.AddSeconds(30)) {
                    $valid = $true
                    break
                }
            }
        } catch {}
    }
}

if (-not $valid) {
    [Console]::Error.WriteLine("[ERROR] AWS STS session for $RoleName has expired or is not initialized.")
    [Console]::Error.WriteLine("[ACTION REQUIRED] Unattended MFA refresh is not supported. Please run in your terminal:")
    [Console]::Error.WriteLine("  aws sts get-caller-identity --profile $Profile")
    exit 1
}

& "C:\Program Files\Amazon\AWSCLIV2\aws.exe" configure export-credentials --profile $Profile --format process
exit $LASTEXITCODE
