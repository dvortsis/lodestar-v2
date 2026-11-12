#Requires -Version 7.2
<#
.SYNOPSIS
    Owner-run interactive MFA refresh and read-only fleet inspection.
.DESCRIPTION
    Run with & from your own PowerShell console. Prompts for a read-only Linode
    PAT without echoing it. Never saves the PAT, raw Terraform state, or SSH
    keys. AWS CLI uses its normal MFA/STS session cache; credential responses
    are excluded from the selected-field report.
    No Terraform command, PIA secret read, SSH connection, or cloud write occurs.
    Backend environment settings remain in THIS terminal, not other terminals
    or the agent process. The report is the handoff to the agent.
.PARAMETER StateProfile
    Existing AWS profile authorized to READ the shared S3 state. No automatic
    administrator fallback or IAM/profile creation. Observer may lack S3 access.
.PARAMETER StateBucket
    Existing fleet-control stack StateBucketName output. Never a new bucket.
.PARAMETER ControlBucket
    Existing fleet-control stack ControlBucketName output.
#>
[CmdletBinding()]
param(
    [string]$StateProfile,
    [string]$StateBucket = $env:LODESTAR_TF_STATE_BUCKET,
    [string]$StateRegion = $env:LODESTAR_TF_STATE_REGION,
    [string]$ControlBucket = $env:CONTROL_BUCKET
)

function Get-InspectionField($Object, [string]$Name, $Default = $null) {
    if ($null -eq $Object) { return $Default }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $Default }
    return $property.Value
}

function Get-AwsInspectionSetting([string]$Profile, [ValidateSet('source_profile', 'login_session')][string]$Name) {
    # Read only these non-secret settings; never read/export access keys.
    $result = Invoke-AwsCli @('configure', 'get', $Name, '--profile', $Profile)
    if ($result.ExitCode -eq 0) { return $result.Out.Trim() }
    return $null
}

function Get-ExistingAwsLoginSource([string]$Profile) {
    $seen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $current = $Profile
    for ($depth = 0; $depth -lt 16; $depth++) {
        if (-not $seen.Add($current)) { throw 'AWS source-profile chain contains a cycle.' }
        if ($current -in @('lodestar-admin', 'lodestar-provisioner')) {
            throw 'Refusing authentication through an administrator/provisioner source profile.'
        }
        $source = Get-AwsInspectionSetting $current source_profile
        if ($source) { $current = $source; continue }
        $session = Get-AwsInspectionSetting $current login_session
        if ($session) { return @{ profile = $current; session = $session } }
        # Static credentials, SSO and credential_process retain their own workflows.
        # In particular, never run aws login on the observer role or a new/default profile.
        return $null
    }
    throw 'AWS source-profile chain is unexpectedly deep.'
}

function Initialize-ExistingAwsInspectionSession([string]$Profile) {
    $login = Get-ExistingAwsLoginSource $Profile
    if ($null -ne $login) {
        $check = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', $login.profile,
            '--query', 'Arn', '--output', 'text', '--no-cli-pager')
        if ($check.ExitCode -ne 0) {
            $versionResult = Invoke-AwsCli @('--version')
            $versionMatch = [regex]::Match(($versionResult.Out + $versionResult.Err), 'aws-cli/(\d+\.\d+\.\d+)')
            if (-not $versionMatch.Success -or [version]$versionMatch.Groups[1].Value -lt [version]'2.32.0') {
                throw 'Refreshing an AWS console login requires AWS CLI 2.32.0 or newer.'
            }
            Write-Host "The existing console login for source profile '$($login.profile)' needs reauthentication."
            Write-Host "Proposed command: aws login --profile $($login.profile)"
            Write-Host 'In the browser select that existing identity. Decline any prompt to switch to another identity.'
            $consent = Read-Host 'Type LOGIN to refresh this existing source session, or Enter to stop'
            if ($consent -cne 'LOGIN') { throw 'Source-session login was not authorized.' }
            & $script:AwsCli login --profile $login.profile --no-cli-pager
            if ($LASTEXITCODE -ne 0) { throw 'Existing source-session sign-in failed.' }
            $after = Get-AwsInspectionSetting $login.profile login_session
            if ($after -cne $login.session) {
                throw 'AWS changed the configured source login identity. Stop and review the profile before continuing.'
            }
            $verified = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', $login.profile,
                '--query', 'Arn', '--output', 'text', '--no-cli-pager')
            if ($verified.ExitCode -ne 0) { throw 'Source identity verification failed after sign-in.' }
        }
    }
    # Role MFA is separate from the source browser login. Keep its prompt visible.
    Write-Host "Checking '$Profile'; enter its role MFA in this console if AWS requests it."
    & $script:AwsCli sts get-caller-identity --profile $Profile --query Arn --output text --no-cli-pager
    if ($LASTEXITCODE -ne 0) { throw 'AWS inspection profile authentication failed.' }
}

function Assert-ReadOnlyLinodeScopes([string]$Scopes) {
    $parts = @($Scopes -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    if ($parts.Count -eq 0 -or @($parts | Where-Object { $_ -cnotmatch '^[a-z_]+:read_only$' }).Count) {
        throw 'Linode token refused: missing scope evidence or a scope permits writes. Use a dedicated read-only PAT.'
    }
    return ,$parts
}

function Get-ReadOnlyLinodeInventory {
    param(
        [ValidateSet('profile', 'linode/instances', 'networking/firewalls', 'volumes', 'networking/ips')]
        [string]$Path,
        [string]$Token
    )
    $items = [System.Collections.Generic.List[object]]::new()
    $page = 1
    $expectedPages = $null
    do {
        $scopeHeaders = $null
        $uri = "https://api.linode.com/v4/$Path"
        if ($Path -ne 'profile') { $uri += "?page_size=100&page=$page" }
        try {
            $response = Invoke-RestMethod -Method Get -TimeoutSec 30 -ErrorAction Stop `
                -Uri $uri `
                -Headers @{ Authorization = "Bearer $Token" } -ResponseHeadersVariable scopeHeaders
        } catch {
            # Do not echo HTTP exception bodies, request headers, or the token.
            return @{ status = 'unavailable'; reason = 'Linode GET failed; verify read scopes and user visibility.' }
        }
        # Akamai documents X-OAuth-Scopes as the PAT's actual allowed scopes:
        # https://techdocs.akamai.com/linode-api/reference/response-headers
        try { $scopes = Assert-ReadOnlyLinodeScopes (($scopeHeaders['X-OAuth-Scopes']) -join ',') }
        catch { return @{ status = 'refused'; reason = 'Token scope evidence is missing or permits writes. Use a dedicated read-only PAT.' } }
        if ($Path -eq 'profile') { return @{ status = 'ok'; scopes = $scopes } }
        $pages = Get-InspectionField $response 'pages'
        $responsePage = Get-InspectionField $response 'page'
        if ($null -eq $response.PSObject.Properties['data'] -or $null -eq $pages -or
            [int]$pages -lt 1 -or [int]$pages -gt 10000 -or [int]$responsePage -ne $page) {
            return @{ status = 'unavailable'; reason = 'Malformed pagination; no empty-inventory inference.' }
        }
        if ($null -ne $expectedPages -and [int]$pages -ne $expectedPages) {
            return @{ status = 'unavailable'; reason = 'Inventory changed during pagination; repeat inspection.' }
        }
        $expectedPages = [int]$pages
        foreach ($item in $response.data) { $items.Add($item) }
        $page++
    } while ($page -le $expectedPages)
    return @{ status = 'ok'; items = $items.ToArray(); scopes = $scopes }
}

function Get-ReadOnlyS3Json([string]$Bucket, [string]$Key, [string]$Region, [string]$Profile, [string]$Account) {
    if (-not $Bucket) { return @{ status = 'not_configured' } }
    $head = Invoke-AwsCli @('s3api', 'head-object', '--bucket', $Bucket, '--key', $Key,
        '--expected-bucket-owner', $Account, '--region', $Region, '--profile', $Profile, '--no-cli-pager')
    if ($head.ExitCode -ne 0) {
        if (($head.Err + $head.Out) -match '\(404\)|\(NoSuchKey\)|\(NotFound\)') {
            return @{ status = 'missing' }
        }
        return @{ status = 'unavailable'; reason = 'S3 read denied or failed; never interpreted as an empty fleet.' }
    }
    # Stream into process memory only. Raw state may contain SSH host private keys.
    $read = Invoke-AwsCli @('s3', 'cp', "s3://$Bucket/$Key", '-', '--only-show-errors',
        '--region', $Region, '--profile', $Profile, '--no-cli-pager')
    if ($read.ExitCode -ne 0) { return @{ status = 'unavailable'; reason = 'S3 object read failed.' } }
    try { $document = ConvertFrom-Json -InputObject $read.Out -ErrorAction Stop }
    catch { return @{ status = 'unavailable'; reason = 'S3 object is not valid JSON.' } }
    if ($null -eq $document) { return @{ status = 'unavailable'; reason = 'S3 object is empty.' } }
    return @{ status = 'ok'; document = $document }
}

function ConvertTo-FleetInspectionSummary($Read, [ValidateSet('state', 'fleet', 'lock', 'mismatch', 'ledger')][string]$Kind) {
    if ($Read.status -ne 'ok') { return @{ status = $Read.status } }
    $doc = $Read.document
    switch ($Kind) {
        'state' {
            if ((Get-InspectionField $doc 'version') -ne 4 -or $null -eq $doc.PSObject.Properties['resources']) {
                return @{ status = 'unavailable'; reason = 'Unexpected Terraform state schema.' }
            }
            $resources = @(foreach ($resource in $doc.resources) {
                foreach ($instance in @(Get-InspectionField $resource 'instances' @())) {
                    $entry = [ordered]@{ type = $resource.type; name = $resource.name }
                    if ($resource.type -in @('linode_instance', 'linode_firewall')) {
                        $entry.id = Get-InspectionField $instance.attributes 'id'
                        $entry.label = Get-InspectionField $instance.attributes 'label'
                    }
                    [pscustomobject]$entry
                }
            })
            return @{ status = 'ok'; serial = $doc.serial; resources = $resources }
        }
        'fleet' {
            if ((Get-InspectionField $doc 'version') -ne 1) { return @{ status = 'unavailable' } }
            $fleetProperty = $doc.PSObject.Properties['fleet']
            if ($null -ne $fleetProperty -and $null -eq $fleetProperty.Value) {
                return @{ status = 'ok'; fleet = 'tombstone' }
            }
            if ($null -eq $doc.PSObject.Properties['roles'] -or (Get-InspectionField $doc 'commit') -cnotmatch '^[0-9a-f]{40}$') {
                return @{ status = 'unavailable' }
            }
            return @{ status = 'ok'; fleet = 'recorded'; commit = $doc.commit;
                expires_at = Get-InspectionField $doc 'expires_at'; roles = @($doc.roles.PSObject.Properties.Name) }
        }
        'lock' { return @{ status = 'present'; kind = Get-InspectionField $doc 'kind'; expires_at = Get-InspectionField $doc 'expires_at' } }
        'mismatch' { return @{ status = 'present'; review_required = $true } }
        'ledger' {
            $month = [DateTime]::UtcNow.ToString('yyyy-MM')
            $amount = Get-InspectionField $doc $month
            if ($null -eq $amount) { return @{ status = 'ok'; month = $month; recorded_spend_usd = $null } }
            try { return @{ status = 'ok'; month = $month; recorded_spend_usd = [decimal]$amount } }
            catch { return @{ status = 'unavailable' } }
        }
    }
}

function Invoke-ReadOnlyFleetAccess {
    param([string]$StateProfile, [string]$StateBucket, [string]$StateRegion, [string]$ControlBucket)
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    . (Join-Path $PSScriptRoot 'Interactive.ps1')
    Assert-InteractiveConfirmation
    . (Join-Path $PSScriptRoot 'AwsCli.ps1')
    . (Join-Path $PSScriptRoot 'StateBackend.ps1')
    $report = [ordered]@{
        captured_at = [DateTime]::UtcNow.ToString('o')
        status = 'incomplete'
        provisioning_authorized = $false
        visibility = 'API-visible inventory only; owner must confirm account-wide visibility and reconcile state.'
        observer = @{ status = 'unavailable' }
        aws_instances = @{ status = 'unavailable' }
        backend = @{ status = 'unavailable' }
        linode = @{ status = 'unavailable' }
    }
    $token = $null
    $secureToken = $null
    $previousBucket = $env:LODESTAR_TF_STATE_BUCKET
    $previousRegion = $env:LODESTAR_TF_STATE_REGION
    $backendValidated = $false
    $stage = 'observer authentication (source login and role MFA)'
    $root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
    $reportPath = Join-Path $root 'logs/read-only-fleet-report.json'
    try {
        Initialize-ExistingAwsInspectionSession 'lodestar-observer'
        $identityResult = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', 'lodestar-observer', '--output', 'json', '--no-cli-pager')
        if ($identityResult.ExitCode -ne 0) { throw 'Observer identity unavailable.' }
        $identity = $identityResult.Out | ConvertFrom-Json
        if ($identity.Arn -notmatch ':assumed-role/LodestarObserver/') { throw 'Unexpected observer identity.' }
        $report.observer = @{ status = 'ok'; profile = 'lodestar-observer'; role = 'LodestarObserver' }
        $inventory = Invoke-AwsCli @('ec2', 'describe-instances', '--profile', 'lodestar-observer', '--region', 'us-east-1',
            '--filters', 'Name=tag:Project,Values=Lodestar', '--query',
            'Reservations[].Instances[].{id:InstanceId,state:State.Name,type:InstanceType}', '--output', 'json', '--no-cli-pager')
        if ($inventory.ExitCode -eq 0) { $report.aws_instances = @{ status = 'ok'; items = @($inventory.Out | ConvertFrom-Json) } }

        $stage = 'backend configuration'
        Write-Host 'Use existing fleet-control StateBucketName/ControlBucketName outputs. Do not create a bucket.'
        if (-not $StateBucket) { $StateBucket = Read-Host 'Existing shared state bucket' }
        if (-not $StateRegion) { $StateRegion = Read-Host 'Existing bucket AWS region (for example us-east-1)' }
        if (-not $ControlBucket) { $ControlBucket = Read-Host 'Existing control bucket (Enter if unknown)' }
        if (-not $StateProfile) { $StateProfile = Read-Host 'Existing state-read AWS profile (Enter for lodestar-observer)' }
        if (-not $StateProfile) { $StateProfile = 'lodestar-observer' }
        if ($StateProfile -in @('lodestar-admin', 'lodestar-provisioner', 'lodestar-agent')) { throw 'Use an existing state-read profile, not an administrator/provisioner/base-agent profile.' }
        $profiles = Invoke-AwsCli @('configure', 'list-profiles')
        if ($profiles.ExitCode -ne 0 -or $StateProfile -notin @($profiles.Out -split '\r?\n')) { throw 'State-read profile is not configured.' }
        $env:LODESTAR_TF_STATE_BUCKET = $StateBucket
        $env:LODESTAR_TF_STATE_REGION = $StateRegion
        $null = Get-StateBackendArgs
        if ($ControlBucket -and $ControlBucket -cnotmatch '^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]\z') { throw 'Invalid control bucket.' }
        $backendValidated = $true
        $configuration = @{ bucket = $StateBucket; region = $StateRegion; control_bucket = $ControlBucket; profile = $StateProfile }
        $report.backend = @{ status = 'unavailable'; configuration = $configuration }
        # State-reader authentication errors must not hide the remaining Linode inventory.
        try {
            Initialize-ExistingAwsInspectionSession $StateProfile
            $reader = Invoke-AwsCli @('sts', 'get-caller-identity', '--profile', $StateProfile, '--output', 'json', '--no-cli-pager')
        } catch { $reader = [pscustomobject]@{ ExitCode = 1; Out = ''; Err = '' } }
        if ($reader.ExitCode -eq 0 -and (($reader.Out | ConvertFrom-Json).Account -eq $identity.Account)) {
            $report.backend = [ordered]@{ status = 'inspected'; key = 'terraform/lodestar.tfstate'; configuration = $configuration }
            foreach ($item in @(
                @{ name = 'terraform'; bucket = $StateBucket; key = 'terraform/lodestar.tfstate'; kind = 'state' },
                @{ name = 'fleet'; bucket = $StateBucket; key = 'fleet/fleet.json'; kind = 'fleet' },
                @{ name = 'ledger'; bucket = $StateBucket; key = 'fleet/spend-ledger.json'; kind = 'ledger' },
                @{ name = 'lease'; bucket = $ControlBucket; key = 'locks/fleet.json'; kind = 'lock' },
                @{ name = 'mismatch'; bucket = $ControlBucket; key = 'locks/fleet-mismatch.json'; kind = 'mismatch' }
            )) {
                $read = Get-ReadOnlyS3Json $item.bucket $item.key $StateRegion $StateProfile $identity.Account
                $report.backend[$item.name] = ConvertTo-FleetInspectionSummary $read $item.kind
            }
        }

        $stage = 'Linode read-only token and inventory'
        Write-Host 'Use a short-lived PAT with ONLY read-only scopes: linodes, firewall, volumes, ips; optionally account/images.'
        $secureToken = Read-Host 'Read-only Linode PAT (hidden; not saved)' -AsSecureString
        $token = [Net.NetworkCredential]::new('', $secureToken).Password
        if ([string]::IsNullOrWhiteSpace($token)) { throw 'No token supplied.' }
        $auth = Get-ReadOnlyLinodeInventory -Path profile -Token $token
        if ($auth.status -ne 'ok') {
            $report.linode = @{ status = $auth.status; reason = $auth.reason }
            throw 'Linode authentication or read-only scope validation unavailable.'
        }
        $report.linode = [ordered]@{ status = 'inspected'; verified_scopes = $auth.scopes }
        foreach ($path in @('linode/instances', 'networking/firewalls', 'volumes', 'networking/ips')) {
            $read = Get-ReadOnlyLinodeInventory -Path $path -Token $token
            if ($read.status -eq 'ok') {
                # Explicit allowlist: never serialize the raw profile, HTTP response, or credentials.
                $fields = switch ($path) {
                    'linode/instances' { @('id', 'label', 'status', 'type', 'region', 'tags') }
                    'networking/firewalls' { @('id', 'label', 'status', 'tags') }
                    'volumes' { @('id', 'label', 'status', 'linode_id', 'size', 'region', 'tags') }
                    'networking/ips' { @('address', 'type', 'public', 'linode_id', 'region') }
                }
                $report.linode[$path] = @{ status = 'ok'; items = @($read.items | Select-Object -Property $fields) }
            } else { $report.linode[$path] = @{ status = $read.status } }
        }
        $complete = $report.aws_instances.status -eq 'ok' -and $report.backend.status -eq 'inspected'
        if ($report.backend.status -eq 'inspected') {
            foreach ($name in @('terraform', 'fleet', 'ledger')) { $complete = $complete -and $report.backend[$name].status -eq 'ok' }
            foreach ($name in @('lease', 'mismatch')) { $complete = $complete -and $report.backend[$name].status -in @('present', 'missing') }
        }
        foreach ($path in @('linode/instances', 'networking/firewalls', 'volumes', 'networking/ips')) {
            $complete = $complete -and $report.linode[$path].status -eq 'ok'
        }
        $report.status = if ($complete) { 'inspection_finished_review_required' } else { 'incomplete' }
    } catch {
        # Error bodies may contain sensitive data. A stage marker is sufficient for handoff.
        $report.status = 'incomplete'
        $report.failure_stage = $stage
        Write-Warning "Inspection stopped during $stage. Failed reads never mean an empty fleet. See the selected-field report."
    } finally {
        $token = $null
        if ($null -ne $secureToken) { $secureToken.Dispose() }
        if (-not $backendValidated) {
            $env:LODESTAR_TF_STATE_BUCKET = $previousBucket
            $env:LODESTAR_TF_STATE_REGION = $previousRegion
        }
        $logs = Split-Path -Parent $reportPath
        if (-not (Test-Path -LiteralPath $logs)) { $null = New-Item -ItemType Directory -Path $logs }
        foreach ($path in @($logs, $reportPath)) {
            if ((Test-Path -LiteralPath $path) -and ((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw 'Refusing a reparse-point report destination.'
            }
        }
        $report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $reportPath -Encoding utf8
        Write-Host "Selected-field report (contains resource metadata, no credentials): $reportPath"
        Write-Host 'Linode PAT was not stored in environment variables or files. AWS CLI uses its normal session cache.'
        Write-Host 'No Terraform or cloud-write commands ran.'
        Write-Host 'Backend variables apply only to this terminal. Other terminals/agents must use the report or their own setup.'
    }
    if ($report.status -eq 'incomplete') { throw 'Inspection incomplete. Review the report; do not provision or assume an idle fleet.' }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-ReadOnlyFleetAccess -StateProfile $StateProfile -StateBucket $StateBucket -StateRegion $StateRegion -ControlBucket $ControlBucket
}
