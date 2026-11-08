# Get-McpCredentials.ps1
# Fail-fast credential process wrapper for AWS MCP with MFA caching.
param(
    [string]$RoleName = "LodestarObserver",
    [Alias('Profile')]
    [string]$ProfileName = "lodestar-observer"
)

$cacheDir = Join-Path $HOME ".aws\cli\cache"
$valid = $false
$now = [DateTime]::UtcNow

if (Test-Path $cacheDir) {
    $cacheFiles = Get-ChildItem -Path $cacheDir -Filter "*.json" -File
    foreach ($file in $cacheFiles) {
        try {
            $data = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
            if ($data.AssumedRoleUser.Arn -match ('role/' + [regex]::Escape($RoleName) + '/')) {
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
    [Console]::Error.WriteLine("  aws sts get-caller-identity --profile $ProfileName")
    exit 1
}

$awsCli = 'C:\Program Files\Amazon\AWSCLIV2\aws.exe'
if (-not (Test-Path -LiteralPath $awsCli)) {
    $cmd = Get-Command aws -ErrorAction SilentlyContinue
    if ($cmd) { $awsCli = $cmd.Source } else { [Console]::Error.WriteLine('[ERROR] AWS CLI v2 executable not found.'); exit 1 }
}
& $awsCli configure export-credentials --profile $ProfileName --format process
exit $LASTEXITCODE
