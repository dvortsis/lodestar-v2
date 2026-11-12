<#
.SYNOPSIS
    Offline test for scripts/Deploy-AwsWindows.ps1 (X-test-infra-blockers).
.DESCRIPTION
    No network, no AWS, no credentials. It runs a temporary copy of the scripts
    with -DryRun against a stub `aws` that records its arguments, on a PATH that
    holds only the stub's folder and /usr/bin:/bin (so no real AWS CLI can be
    found), with AWS's config and credential files pointed at nothing. It checks:
    the launch asks for a public IPv4 on one network interface with the approved
    subnet and group; the security group's inbound rules are checked first and
    any rule, a failed call or an odd answer refuses the launch; the existing
    guards still hold (an active instance, piped confirmation input).
    Linux or macOS pwsh only (the stub is a shell script).
    Run: pwsh -NoProfile -File infra/akamai/terraform/tests/Test-DeployAwsWindows.ps1
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSEdition -ne 'Core' -or $IsWindows) {
    Write-Host 'skip: this test needs pwsh on Linux or macOS (its AWS CLI stub is a shell script)'
    exit 0
}

$scriptsDir = Join-Path (Join-Path $PSScriptRoot '..') 'scripts'
$pwshExe = (Get-Process -Id $PID).Path
$SG = 'sg-0123456789abcdef0'
$SUBNET = 'subnet-0123456789abcdef0'
$script:failures = 0

$stubBody = @'
#!/bin/sh
# Records each call, one line: CALL <arg> <arg> ...
{ printf 'CALL'; for a in "$@"; do printf ' <%s>' "$a"; done; printf '\n'; } >> "$STUB_DIR/calls.log"
case "$1 $2" in
  "sts get-caller-identity")
    for a in "$@"; do
      if [ "$a" = lodestar-admin ]; then
        printf '{"Account":"111122223333","Arn":"arn:aws:iam::111122223333:user/lodestar-admin"}\n'; exit 0
      fi
    done
    printf '{"Account":"111122223333","Arn":"arn:aws:sts::111122223333:assumed-role/LodestarProvisioner/test"}\n' ;;
  "ec2 describe-instances") cat "$STUB_DIR/instances.txt" ;;
  "ec2 describe-security-groups")
    if [ -f "$STUB_DIR/sg.exit" ]; then echo 'An error occurred (UnauthorizedOperation)' >&2; exit "$(cat "$STUB_DIR/sg.exit")"; fi
    cat "$STUB_DIR/sg.txt" ;;
  "ec2 run-instances")
    for a in "$@"; do
      if [ "$a" = --dry-run ]; then
        echo 'An error occurred (DryRunOperation) when calling the RunInstances operation: Request would have succeeded, but DryRun flag is set.' >&2
        exit 254
      fi
    done
    echo 'stub: a live launch was attempted' >&2; exit 99 ;;
  *) echo "stub: unexpected call $1 $2" >&2; exit 98 ;;
esac
'@

function New-Sandbox {
    $root = Join-Path ([System.IO.Path]::GetTempPath()) ("deploy-aws-test-" + [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path (Join-Path $root 'scripts'), (Join-Path $root 'stub')
    Copy-Item -Path (Join-Path $scriptsDir '*.ps1') -Destination (Join-Path $root 'scripts')
    $stub = Join-Path (Join-Path $root 'stub') 'aws'
    Set-Content -LiteralPath $stub -Value $stubBody -NoNewline
    & chmod 0755 $stub
    Set-Content -LiteralPath (Join-Path $root 'stub/instances.txt') -Value ''
    Set-Content -LiteralPath (Join-Path $root 'stub/sg.txt') -Value "$SG`t0"
    return $root
}

function Invoke-Deploy([string]$Root, [switch]$DryRun, [string]$PipeIn) {
    $saved = @{}
    $vars = @{
        PATH                        = "$(Join-Path $Root 'stub'):/usr/bin:/bin"
        STUB_DIR                    = (Join-Path $Root 'stub')
        AWS_CONFIG_FILE             = '/nonexistent'
        AWS_SHARED_CREDENTIALS_FILE = '/nonexistent'
        AWS_EC2_METADATA_DISABLED   = 'true'
        LODESTAR_AWS_ACCOUNT_ID     = '111122223333'
    }
    foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
    $deploy = Join-Path (Join-Path $Root 'scripts') 'Deploy-AwsWindows.ps1'
    $argv = @('-NoProfile', '-File', $deploy, '-Hours', '4', '-AmiId', 'ami-0123456789abcdef0', '-SubnetId', $SUBNET,
        '-SecurityGroupId', $SG, '-InstanceProfileName', 'LodestarTestProfile')
    if ($DryRun) { $argv += '-DryRun' }
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        if ($PipeIn) { $out = ($PipeIn | & $pwshExe @argv 2>&1) -join "`n" } else { $out = (& $pwshExe @argv 2>&1) -join "`n" }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $eap
        foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
    }
    $log = Join-Path $Root 'stub/calls.log'
    $calls = if (Test-Path -LiteralPath $log) { @(Get-Content -LiteralPath $log) } else { @() }
    [PSCustomObject]@{ Code = $code; Out = $out; Calls = $calls }
}

function Check([string]$Name, [bool]$Ok, [string]$Detail) {
    if ($Ok) { Write-Host "ok   $Name" } else { Write-Host "FAIL $Name : $Detail"; $script:failures++ }
}

function Calls-Of($Result, [string]$Op) { @($Result.Calls | Where-Object { $_.StartsWith("CALL <ec2> <$Op>") }) }

$roots = [System.Collections.Generic.List[string]]::new()
try {
    # 1. No inbound rules: the dry run asks for a public IPv4 on one interface.
    $root = New-Sandbox; $roots.Add($root)
    $r = Invoke-Deploy $root -DryRun
    $run = @(Calls-Of $r 'run-instances')
    $sgCalls = @(Calls-Of $r 'describe-security-groups')
    Check 'dry run passes with no inbound rules' ($r.Code -eq 0 -and $r.Out -match 'PASS: DryRunOperation') "exit $($r.Code): $($r.Out)"
    Check 'the stub was the CLI used' ($r.Calls.Count -ge 4) "calls: $($r.Calls -join ' | ')"
    Check 'one read-only security group check, for the approved group' ($sgCalls.Count -eq 1 -and $sgCalls[0].Contains("<--group-ids> <$SG>") -and $sgCalls[0].Contains('<--profile> <lodestar-provisioner>')) ($sgCalls -join ' | ')
    Check 'one run-instances call, a dry run' ($run.Count -eq 1 -and $run[0].Contains('<--dry-run>')) ($run -join ' | ')
    $nic = "<--network-interfaces> <DeviceIndex=0,SubnetId=$SUBNET,Groups=$SG,AssociatePublicIpAddress=true,DeleteOnTermination=true>"
    Check 'one interface: same subnet and group, public IPv4' ($run.Count -eq 1 -and $run[0].Contains($nic)) ($run -join ' | ')
    Check 'no --subnet-id or --security-group-ids beside it' ($run.Count -eq 1 -and -not $run[0].Contains('<--subnet-id>') -and -not $run[0].Contains('<--security-group-ids>')) ($run -join ' | ')
    Check 'the other launch arguments are unchanged' ($run.Count -eq 1 -and $run[0].Contains('<--metadata-options> <HttpTokens=required>') -and
        $run[0].Contains('<--instance-type> <c6a.xlarge>') -and $run[0].Contains('Encrypted=true') -and
        $run[0].Contains('<--iam-instance-profile> <Arn=arn:aws:iam::111122223333:instance-profile/LodestarTestProfile>')) ($run -join ' | ')
    $sgIndex = [array]::FindIndex([string[]]$r.Calls, [Predicate[string]] { param($c) $c.StartsWith('CALL <ec2> <describe-security-groups>') })
    $runIndex = [array]::FindIndex([string[]]$r.Calls, [Predicate[string]] { param($c) $c.StartsWith('CALL <ec2> <run-instances>') })
    Check 'the group is checked before the launch' ($sgIndex -ge 0 -and $sgIndex -lt $runIndex) "order $sgIndex, $runIndex"
    Check 'it says a public IPv4 is attached, outbound only' ($r.Out -match 'Public IPv4:\s+attached at launch' -and $r.Out -match 'reachable outbound only') $r.Out
    Check 'the plan names the public IPv4 charge' ($r.Out -match 'public IPv4 is billed on top') $r.Out

    # 2. Refusals: any inbound rule, a failed call, an odd answer. No launch, not even a dry run.
    foreach ($case in @(
            @{ Name = 'one inbound rule'; Sg = "$SG`t1"; Expect = "has 1 inbound rule\(s\)" },
            @{ Name = 'several inbound rules'; Sg = "$SG`t3"; Expect = "has 3 inbound rule\(s\)" },
            @{ Name = 'the call fails'; Exit = '254'; Expect = 'Could not read security group' },
            @{ Name = 'no such group'; Sg = ''; Expect = 'Unexpected answer' },
            @{ Name = 'another group answers'; Sg = "sg-0fedcba98765432100`t0"; Expect = 'Unexpected answer' },
            @{ Name = 'two groups answer'; Sg = "$SG`t0`n$SG`t0"; Expect = 'Unexpected answer' },
            @{ Name = 'a non-number count'; Sg = "$SG`tNone"; Expect = 'Unexpected answer' })) {
        $root = New-Sandbox; $roots.Add($root)
        if ($case.ContainsKey('Exit')) { Set-Content -LiteralPath (Join-Path $root 'stub/sg.exit') -Value $case.Exit }
        else { Set-Content -LiteralPath (Join-Path $root 'stub/sg.txt') -Value $case.Sg }
        $r = Invoke-Deploy $root -DryRun
        Check "refused: $($case.Name)" ($r.Code -ne 0 -and $r.Out -match $case.Expect -and $r.Out -match 'No deployment performed') "exit $($r.Code): $($r.Out)"
        Check "no launch: $($case.Name)" (@(Calls-Of $r 'run-instances').Count -eq 0) ($r.Calls -join ' | ')
    }

    # 3. The existing guards still hold.
    $root = New-Sandbox; $roots.Add($root)
    Set-Content -LiteralPath (Join-Path $root 'stub/instances.txt') -Value "i-0123456789abcdef0`trunning`t2026-11-12T00:00:00Z"
    $r = Invoke-Deploy $root -DryRun
    Check 'an active instance still halts the run' ($r.Code -ne 0 -and $r.Out -match 'Deployment halted') $r.Out
    Check 'nothing launched beside an active instance' (@(Calls-Of $r 'run-instances').Count -eq 0) ($r.Calls -join ' | ')

    $root = New-Sandbox; $roots.Add($root)
    $r = Invoke-Deploy $root -PipeIn 'DEPLOY 1 AWS WINDOWS INSTANCE'
    Check 'a piped confirmation phrase is refused' ($r.Code -ne 0 -and $r.Out -match 'Refusing to read the confirmation phrase') $r.Out
    Check 'no launch without a typed phrase' (@(Calls-Of $r 'run-instances').Count -eq 0) ($r.Calls -join ' | ')
    Check 'no state written without a typed phrase' (-not (Test-Path -LiteralPath (Join-Path $root 'state/aws-fleet-state.json'))) 'state file exists'
} finally {
    foreach ($d in $roots) { Remove-Item -LiteralPath $d -Recurse -Force -ErrorAction SilentlyContinue }
}

if ($script:failures -gt 0) { Write-Host "$($script:failures) failure(s)"; exit 1 }
Write-Host 'all passed'
