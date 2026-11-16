param(
    [string]$ProposalAttachment,
    [string]$OutputDirectory = (Join-Path ([IO.Path]::GetTempPath()) ('lodestar-scheduler-review-' + [guid]::NewGuid().ToString('N'))),
    [switch]$SyncEvidence
)
$ErrorActionPreference = 'Stop'
if ((& rustc --version) -notmatch '^rustc 1\.99\.1 ' -or
    (& cargo --version) -notmatch '^cargo 1\.99\.1 ') {
    throw 'Use the recorded Rust/Cargo 1.99.1 toolchain; this script does not install toolchains.'
}
$expectedHash = 'AFEB690C3765BF07E67A1485D96527CB2BF4BBDFEE60429EA0993F0CF7AC0E68'
if ($ProposalAttachment) {
    if ((Get-FileHash -LiteralPath $ProposalAttachment -Algorithm SHA256).Hash -ne $expectedHash) {
        throw 'Attachment differs from the reviewed proposal.'
    }
    $pasted = Get-Content -LiteralPath $ProposalAttachment -Raw
    $match = [regex]::Match($pasted, '(?s)```rust\r?\n(.*?)\r?\n```')
    if (-not $match.Success) { throw 'No Rust code block.' }
    $original = $match.Groups[1].Value.Replace("`r`n", "`n")
} else {
    $original = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'original-scheduler.rs.txt') -Raw).Replace("`r`n", "`n")
}
$sha = [Security.Cryptography.SHA256]::Create()
try {
    $moduleHash = [Convert]::ToHexString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($original)))
} finally { $sha.Dispose() }
if ($moduleHash -ne '044754DD300000D7E1D612514E8C7D4153382C21F91B3772B9F590B6435D824C') {
    throw 'Extracted scheduler differs from the reviewed module.'
}
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Use a new output directory.' }
New-Item -ItemType Directory -Path (Join-Path $OutputDirectory 'src') | Out-Null
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'src/scheduler.rs'), $original)
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'src/lib.rs'), "pub mod scheduler;`n")
@'
[package]
name = "scheduler-review-harness"
version = "0.1.0"
edition = "2021"
[dependencies]
rusqlite = { version = "=0.40.2", features = ["bundled"] }
tokio = { version = "=1.53.3", features = ["sync", "rt-multi-thread", "macros", "time"] }
tracing = "=0.1.44"
[dev-dependencies]
tempfile = "=3.27.0"
'@ | Set-Content -LiteralPath (Join-Path $OutputDirectory 'Cargo.toml')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'Cargo.lock') -Destination (Join-Path $OutputDirectory 'Cargo.lock')
$manifest = Join-Path $OutputDirectory 'Cargo.toml'
function Scrub-LogText([string]$Text) {
    $scrubbed = $Text
    $tempNorm = $OutputDirectory.TrimEnd('\', '/')
    $tempSlash = $tempNorm.Replace('\', '/')
    foreach ($variant in @("\\?\$tempNorm", "\\?/$tempSlash", $tempNorm, $tempSlash)) {
        if ($variant) { $scrubbed = $scrubbed.Replace($variant, '<TEMP_VERIFY_DIR>') }
    }
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path.TrimEnd('\', '/')
    $repoSlash = $repoRoot.Replace('\', '/')
    foreach ($variant in @("\\?\$repoRoot", $repoRoot, $repoSlash)) {
        if ($variant) { $scrubbed = $scrubbed.Replace($variant, '<REPO_ROOT>') }
    }
    $userProfile = $env:USERPROFILE
    if ($userProfile) {
        $upNorm = $userProfile.TrimEnd('\', '/')
        $upSlash = $upNorm.Replace('\', '/')
        foreach ($variant in @($upNorm, $upSlash)) {
            if ($variant) { $scrubbed = $scrubbed.Replace($variant, '<USER_HOME>') }
        }
    }
    return $scrubbed
}
function Run-Check([string]$Log, [string[]]$Arguments) {
    $output = & cargo @Arguments 2>&1
    $code = $LASTEXITCODE
    $scrubbedLines = @($output | ForEach-Object { (Scrub-LogText ($_.ToString())).TrimEnd() })
    while ($scrubbedLines.Count -gt 0 -and $scrubbedLines[-1] -eq '') {
        if ($scrubbedLines.Count -eq 1) { $scrubbedLines = @(); break }
        $scrubbedLines = $scrubbedLines[0..($scrubbedLines.Count - 2)]
    }
    [IO.File]::WriteAllText((Join-Path $OutputDirectory $Log), ($scrubbedLines -join "`r`n") + "`r`n")
    Write-Output "$Log exit=$code"
    return $code
}
$code = Run-Check '01-original-build.log' @('test','--manifest-path',$manifest,'--locked','--offline','--lib')
if ($code[-1] -eq 0) { throw 'Expected original borrow-checker errors did not reproduce.' }
if ((Get-Content -LiteralPath (Join-Path $OutputDirectory '01-original-build.log') -Raw) -notmatch 'E0499') {
    throw 'Original build failed for a reason other than the expected borrow errors; inspect the log.'
}
# Mechanical borrow-scope changes only. No scheduling, SQL or clock policy changes.
$patched = $original.Replace('            core.next_op_seq = assigned_seq;', '')
$patched = $patched.Replace('            core.in_flight_count += 1;', '')
$patched = $patched.Replace("            (`n                addr,`n                assigned_seq,", "            let support = rec.support;`n            let last_novel = rec.last_novel_hashes;`n            core.next_op_seq = assigned_seq;`n            core.in_flight_count += 1;`n            (`n                addr,`n                assigned_seq,")
$patched = $patched.Replace("                rec.support,`n                rec.last_novel_hashes,", "                support,`n                last_novel,")
$patched = $patched.Replace('            core.next_op_seq = completion_seq;', '')
$patched = $patched.Replace('            rec.lifecycle = Lifecycle::Completing { completion_seq };', "            rec.lifecycle = Lifecycle::Completing { completion_seq };`n            core.next_op_seq = completion_seq;")
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'src/scheduler.rs'), $patched)
$code = Run-Check '02-supplied-tests.log' @('test','--manifest-path',$manifest,'--locked','--offline','--lib','scheduler::tests')
if ($code[-1] -ne 0) { throw 'Supplied tests failed after mechanical repair.' }
if ((Get-Content -LiteralPath (Join-Path $OutputDirectory '02-supplied-tests.log') -Raw) -notmatch 'test result: ok\. 10 passed; 0 failed;') {
    throw 'The ten supplied tests were not all executed.'
}
$code = Run-Check '03-clippy-after-borrow-repair.log' @('clippy','--manifest-path',$manifest,'--locked','--offline','--all-targets','--','-D','warnings')
if ($code[-1] -ne 0) { throw 'Clippy failed after mechanical repair.' }
$code = Run-Check '04-format-after-borrow-repair.log' @('fmt','--manifest-path',$manifest,'--','--check')
[IO.File]::AppendAllText((Join-Path $OutputDirectory 'src/scheduler.rs'), "`n" + (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'review-regressions.rs') -Raw))
$code = Run-Check '05-added-regressions.log' @('test','--manifest-path',$manifest,'--locked','--offline','--lib','review_regressions','--','--nocapture')
if ($code[-1] -eq 0) { throw 'Expected clock and multiple-owner counterexamples did not reproduce.' }
$regressionLog = Get-Content -LiteralPath (Join-Path $OutputDirectory '05-added-regressions.log') -Raw
if ($regressionLog -notmatch 'test result: FAILED\. 0 passed; 2 failed;' -or
    $regressionLog -notmatch 'A lost its six-hour cooldown after only ten monotonic seconds' -or
    $regressionLog -notmatch "second scheduler dispatched X during first scheduler's six-hour cooldown") {
    throw 'The two expected invariant failures were not both established; inspect the regression log.'
}
[IO.File]::AppendAllText((Join-Path $OutputDirectory 'src/scheduler.rs'), "`n" + (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'followup-regressions.rs') -Raw))
$code = Run-Check '06-followup-regressions.log' @('test','--manifest-path',$manifest,'--locked','--offline','--lib','followup_review','--','--nocapture')
if ($code[-1] -eq 0) { throw 'Expected follow-up invariant counterexamples did not reproduce.' }
$followupLog = Get-Content -LiteralPath (Join-Path $OutputDirectory '06-followup-regressions.log') -Raw
if ($followupLog -notmatch 'test result: FAILED\. 0 passed; 5 failed;' -or
    $followupLog -notmatch 'redispatched ten seconds before the received interval expired after a failed completion commit' -or
    $followupLog -notmatch 'scheduler B accepted a permit owned by scheduler A with a different database' -or
    $followupLog -notmatch 'ledger remained permanently wedged by an expired hot_heap entry that reclaim_expired_locked never inspects' -or
    $followupLog -notmatch 'explicitly unsupported node was re-queued onto hot_heap with stale Supported novelty and prioritized ahead of ready candidates after failed completion commit' -or
    $followupLog -notmatch 'endpoint was poisoned in RAM for six hours after a failed WAL reservation even though no packet was dispatched') {
    throw 'The five follow-up invariant failures were not all established; inspect the log.'
}
if ($SyncEvidence) {
    $copyMap = @{
        '01-original-build.log' = '01-original-build.txt'
        '02-supplied-tests.log' = '02-supplied-tests.txt'
        '03-clippy-after-borrow-repair.log' = '03-clippy-after-borrow-repair.txt'
        '05-added-regressions.log' = '05-added-regressions.txt'
        '06-followup-regressions.log' = '06-followup-regressions.txt'
    }
    foreach ($kv in $copyMap.GetEnumerator()) {
        Copy-Item -LiteralPath (Join-Path $OutputDirectory $kv.Key) -Destination (Join-Path $PSScriptRoot $kv.Value) -Force
    }
    $fmtLines = Get-Content -LiteralPath (Join-Path $OutputDirectory '04-format-after-borrow-repair.log') -TotalCount 19
    $fmtLines | Set-Content -LiteralPath (Join-Path $PSScriptRoot '04-format-excerpt.txt')
}
Write-Output "Evidence directory: $OutputDirectory"
