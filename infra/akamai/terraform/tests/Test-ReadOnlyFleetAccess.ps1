#Requires -Version 7.2
# Offline tests only: all HTTP/S3 interactions below are fixtures.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../scripts/Initialize-ReadOnlyFleetAccess.ps1')

function Assert([bool]$Condition, [string]$Name) {
    if (-not $Condition) { throw "FAIL: $Name" }
    Write-Host "PASS: $Name"
}

# Regression: an expired aws-login source must be refreshed before role MFA.
# Every native command and profile read in this scope is a fixture.
& {
    $script:AwsCli = 'Invoke-FixtureAws'
    $script:sourceReady = $false
    $script:configuredSession = 'arn:aws:iam::000000000000:user/fixture-agent'
    $script:consent = 'LOGIN'
    $script:switchIdentity = $false
    $script:cycle = $false
    $script:authCalls = [System.Collections.Generic.List[string]]::new()
    function Invoke-AwsCli {
        param([string[]]$Arguments)
        if ($Arguments[0] -eq 'configure' -and $Arguments[1] -eq 'get') {
            $value = switch ("$($Arguments[4])/$($Arguments[2])") {
                'lodestar-observer/source_profile' { 'fixture-agent' }
                'fixture-agent/source_profile' { if ($script:cycle) { 'lodestar-observer' } }
                'fixture-agent/login_session' { $script:configuredSession }
            }
            return [pscustomobject]@{ ExitCode = $(if ($value) {0} else {1}); Out = [string]$value; Err = '' }
        }
        if ($Arguments[0] -eq '--version') {
            return [pscustomobject]@{ ExitCode = 0; Out = 'aws-cli/2.39.5'; Err = '' }
        }
        if ($Arguments[0] -eq 'sts' -and $Arguments[1] -eq 'get-caller-identity') {
            Assert ($Arguments[3] -eq 'fixture-agent') 'source identity check uses configured source only'
            return [pscustomobject]@{ ExitCode = $(if ($script:sourceReady) {0} else {1}); Out = ''; Err = 'Session expired' }
        }
        throw 'Unexpected fixture AWS operation'
    }
    function Invoke-FixtureAws {
        $script:authCalls.Add(($args -join ' '))
        if ($args[0] -eq 'login') {
            Assert ($args[2] -eq 'fixture-agent') 'login targets source, never observer/default'
            $script:sourceReady = $true
            if ($script:switchIdentity) { $script:configuredSession = 'different-identity' }
        } else {
            Assert (($args[0..3] -join ' ') -eq 'sts get-caller-identity --profile lodestar-observer') 'role MFA remains on observer profile'
        }
        $global:LASTEXITCODE = 0
    }
    function Read-Host { param($Prompt) return $script:consent }

    Initialize-ExistingAwsInspectionSession 'lodestar-observer'
    Assert ($script:authCalls.Count -eq 2 -and $script:authCalls[0].StartsWith('login --profile fixture-agent') -and
        $script:authCalls[1].StartsWith('sts get-caller-identity --profile lodestar-observer')) 'expired source login precedes role MFA'

    $script:authCalls.Clear()
    Initialize-ExistingAwsInspectionSession 'lodestar-observer'
    Assert ($script:authCalls.Count -eq 1 -and $script:authCalls[0].StartsWith('sts ')) 'valid source login needs no new sign-in'

    $script:authCalls.Clear()
    $script:sourceReady = $false
    $script:consent = 'NO'
    $refused = $false
    try { Initialize-ExistingAwsInspectionSession 'lodestar-observer' } catch { $refused = $true }
    Assert ($refused -and $script:authCalls.Count -eq 0) 'declining source login performs no authentication command'

    $script:consent = 'LOGIN'
    $script:switchIdentity = $true
    $refused = $false
    try { Initialize-ExistingAwsInspectionSession 'lodestar-observer' } catch { $refused = $true }
    Assert ($refused -and $script:authCalls.Count -eq 1) 'changed source identity stops before role MFA and inventory'

    $script:cycle = $true
    $refused = $false
    try { $null = Get-ExistingAwsLoginSource 'lodestar-observer' } catch { $refused = $true }
    Assert $refused 'cyclic profile chains fail closed'
}

foreach ($scope in @('', '*', 'linodes:read_write', 'linodes:read_only,firewall:read_write')) {
    $refused = $false
    try { $null = Assert-ReadOnlyLinodeScopes $scope } catch { $refused = $true }
    Assert $refused 'missing or writable token scopes refused'
}
Assert ((Assert-ReadOnlyLinodeScopes 'linodes:read_only, ips:read_only').Count -eq 2) 'read-only scopes accepted'

$script:scope = 'linodes:read_only,ips:read_only'
$script:requestedPages = [System.Collections.Generic.List[int]]::new()
$script:httpFailure = $false
$script:malformed = $false
function Invoke-RestMethod {
    param($Method, $TimeoutSec, $ErrorAction, $Uri, $Headers, $ResponseHeadersVariable)
    Assert ($Method -eq 'Get') 'HTTP method is GET'
    if ($script:httpFailure) { throw 'PRIVATE_CANARY transport error' }
    Set-Variable -Name $ResponseHeadersVariable -Scope 1 -Value @{ 'X-OAuth-Scopes' = @($script:scope) }
    $match = [regex]::Match($Uri, '&page=(\d+)')
    $page = if ($match.Success) { [int]$match.Groups[1].Value } else { 1 }
    $script:requestedPages.Add($page)
    if ($script:malformed) { return [pscustomobject]@{ data = @(); pages = 2 } }
    return [pscustomobject]@{ page = $page; pages = 2; data = @([pscustomobject]@{ id = $page }) }
}
$inventory = Get-ReadOnlyLinodeInventory -Path 'linode/instances' -Token 'PRIVATE_CANARY'
Assert ($inventory.status -eq 'ok' -and $inventory.items.Count -eq 2 -and ($script:requestedPages -join ',') -eq '1,2') 'all inventory pages collected'
$script:malformed = $true
$inventory = Get-ReadOnlyLinodeInventory -Path 'linode/instances' -Token 'PRIVATE_CANARY'
Assert ($inventory.status -eq 'unavailable' -and -not $inventory.ContainsKey('items')) 'malformed pagination is unknown, not empty'
$script:malformed = $false
$script:httpFailure = $true
$inventory = Get-ReadOnlyLinodeInventory -Path 'linode/instances' -Token 'PRIVATE_CANARY'
Assert ($inventory.status -eq 'unavailable' -and ($inventory | ConvertTo-Json) -notmatch 'PRIVATE_CANARY') 'HTTP errors do not expose token or become empty inventory'
$script:httpFailure = $false
$script:scope = '*'
$inventory = Get-ReadOnlyLinodeInventory -Path profile -Token 'PRIVATE_CANARY'
Assert ($inventory.status -eq 'refused' -and -not $inventory.ContainsKey('items')) 'actual response with wildcard scopes refused'

$state = @'
{"version":4,"serial":8,"outputs":{"secret":{"value":"PRIVATE_CANARY"}},"resources":[{"type":"linode_instance","name":"workers","instances":[{"attributes":{"id":7,"label":"lodestar-worker","metadata":{"user_data":"PRIVATE_CANARY"},"private_key":"PRIVATE_CANARY"}}]}]}
'@ | ConvertFrom-Json
$summary = ConvertTo-FleetInspectionSummary @{status='ok';document=$state} state
Assert ($summary.resources.Count -eq 1 -and $summary.resources[0].id -eq 7) 'state resource metadata retained'
Assert (($summary | ConvertTo-Json -Depth 10) -notmatch 'PRIVATE_CANARY|private_key|user_data') 'state secrets and outputs excluded from report'
$summary = ConvertTo-FleetInspectionSummary @{status='unavailable'} state
Assert ($summary.status -eq 'unavailable' -and -not $summary.ContainsKey('resources')) 'state read failure cannot masquerade as idle'
$summary = ConvertTo-FleetInspectionSummary @{status='ok';document=([pscustomobject]@{version=4})} state
Assert ($summary.status -eq 'unavailable') 'malformed state cannot masquerade as idle'
$summary = ConvertTo-FleetInspectionSummary @{status='ok';document=([pscustomobject]@{version=1;fleet=$null})} fleet
Assert ($summary.fleet -eq 'tombstone') 'explicit readable tombstone distinguished'
$summary = ConvertTo-FleetInspectionSummary @{status='missing'} fleet
Assert ($summary.status -eq 'missing' -and -not $summary.ContainsKey('fleet')) 'missing manifest is not a tombstone'

$script:s3Code = '403'
function Invoke-AwsCli {
    param([string[]]$Arguments)
    Assert (($Arguments[0..1] -join ' ') -eq 's3api head-object') 'S3 failure uses metadata GET only'
    return [pscustomobject]@{ ExitCode = 1; Out = ''; Err = "An error occurred ($script:s3Code): PRIVATE_CANARY" }
}
$read = Get-ReadOnlyS3Json 'existing-bucket' 'terraform/lodestar.tfstate' 'us-east-1' 'state-reader' '000000000000'
Assert ($read.status -eq 'unavailable' -and ($read | ConvertTo-Json) -notmatch 'PRIVATE_CANARY') 'S3 permission failure remains unknown and redacted'
$script:s3Code = '404'
$read = Get-ReadOnlyS3Json 'existing-bucket' 'fleet/fleet.json' 'us-east-1' 'state-reader' '000000000000'
Assert ($read.status -eq 'missing') 'confirmed object absence distinguished from access denial'
Write-Host 'All offline read-only fleet access tests passed. No real APIs were called.'
