# Pure decisions and injectable cleanup used by the production collector and tests.
function New-SoakClock { [Diagnostics.Stopwatch]::StartNew() }
function Get-SoakUtcNow { [DateTimeOffset]::UtcNow }
function Wait-SoakTick { Start-Sleep -Seconds 1 }
function Get-SoakExecutableVersion([string]$Path) { (& $Path --version | Out-String).Trim() }
function Get-SoakVolumes([string[]]$Volumes) {
    foreach ($volume in $Volumes) {
        $drive = [IO.DriveInfo]::new($volume)
        if (-not $drive.IsReady) { throw 'Storage volume unavailable' }
        @{ volume = $volume; free_bytes = $drive.AvailableFreeSpace; total_bytes = $drive.TotalSize }
    }
}
function Write-SoakText([string]$Path, [string]$Content, [bool]$Append) {
    if ($Append) { [IO.File]::AppendAllText($Path, $Content + [Environment]::NewLine, [Text.UTF8Encoding]::new($false)) }
    else { [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false)) }
}

function Test-SoakLaunchEnvironment([string]$Bind,
    [scriptblock]$Processes = { @(Get-Process -ErrorAction Stop | Select-Object -ExpandProperty ProcessName) },
    [scriptblock]$Listeners = {
        $net = [Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties()
        @{ tcp=@($net.GetActiveTcpListeners()); udp=@($net.GetActiveUdpListeners()) }
    }) {
    $uri = $null
    if (-not [Uri]::TryCreate("http://$Bind", [UriKind]::Absolute, [ref]$uri) -or
        $uri.Host -ne '127.0.0.1' -or $uri.Port -lt 1024 -or $uri.Port -gt 65535 -or
        $uri.AbsolutePath -ne '/' -or $uri.Query -or $uri.Fragment -or $uri.UserInfo) {
        throw 'Use an explicit unprivileged IPv4 loopback HTTP bind.'
    }
    $names = @(& $Processes)
    if ($names | Where-Object { $_ -like 'lodestar*' }) { throw 'Another Lodestar process is running.' }
    if ($names | Where-Object { $_ -in @('Docker Desktop', 'com.docker.backend', 'dockerd', 'docker-proxy') }) {
        throw 'Docker must remain stopped for the native baseline.'
    }
    $ports = & $Listeners
    if ($ports.tcp | Where-Object Port -eq $uri.Port) { throw 'HTTP port is already owned.' }
    if ($ports.udp | Where-Object Port -eq 6881) { throw 'DHT port is already owned.' }
}

function Test-SoakBuildRecord($Record, [string]$ExeHash, [string]$Commit, [string]$Ui) {
    if ($Record.schema_version -ne 1 -or $Record.exe_sha256 -ne $ExeHash -or
        $Record.source_commit -ne $Commit -or $Record.ui -ne $Ui -or
        $Record.exe_sha256 -notmatch '^[0-9a-fA-F]{64}$' -or
        $Record.index_sha256 -notmatch '^[0-9a-fA-F]{64}$' -or
        $Record.source_commit -notmatch '^[0-9a-fA-F]{40}$' -or
        [string]::IsNullOrWhiteSpace($Record.rustc) -or
        [string]::IsNullOrWhiteSpace($Record.cargo) -or
        [string]::IsNullOrWhiteSpace($Record.target)) {
        throw 'Build record is incomplete or does not match the candidate.'
    }
}

function Save-SoakJson($Value, [string]$Path, [scriptblock]$Replace = { param($a, $b) [IO.File]::Move($a, $b, $true) }) {
    $temp = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temp, ($Value | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
        for ($attempt = 0; $attempt -lt 3; $attempt++) {
            try { & $Replace $temp $Path; return } catch {
                if ($attempt -eq 2) { throw }
                Start-Sleep -Milliseconds 50
            }
        }
    } finally {
        if ([IO.File]::Exists($temp)) { [IO.File]::Delete($temp) }
    }
}

function Get-SoakGuardReason($Volumes, [long]$StopBytes, [long]$LogBytes, [long]$MaxLogBytes, [int]$ReadFailures) {
    if ($ReadFailures -ge 3) { return 'disk_monitor_failed' }
    foreach ($volume in $Volumes) {
        if ($volume.free_bytes -lt $StopBytes) { return 'disk_reserve_reached' }
    }
    if ($LogBytes -gt $MaxLogBytes) { return 'redirected_logs_limit' }
    return $null
}

function Test-SoakEndpoint([double]$Elapsed, $EndAt, [bool]$MetricsOk, $BaselineStart) {
    return $MetricsOk -and $null -ne $BaselineStart -and $null -ne $EndAt -and $Elapsed -ge $EndAt
}

function Enable-SoakChildCtrlC {
    # Automation hosts can start PowerShell with the inheritable ignore-Ctrl+C
    # flag set. Installing an application handler does not clear that flag.
    if (-not ('Cr0Soak.Inheritance' -as [type])) {
        Add-Type -Namespace Cr0Soak -Name Inheritance -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool SetConsoleCtrlHandler(IntPtr handler, bool add);
'@
    }
    if (-not [Cr0Soak.Inheritance]::SetConsoleCtrlHandler([IntPtr]::Zero, $false)) {
        throw 'Cannot enable Ctrl+C handling for the child.'
    }
}

function New-SoakWindowsProcessAdapter($Process, [string]$Identity, [string]$SignalScript = (Join-Path $PSScriptRoot 'cr0-signal.ps1')) {
    # Console attachment resets handlers, and CTRL_C delivery is asynchronous.
    # Keep the supervisor outside the target console throughout signaling. Any
    # helper compilation/attachment failure is caught by Stop-SoakChild, which
    # still waits and uses its identity-checked forced-stop fallback.
    return @{
        State = {
            $Process.Refresh()
            @{ identity = if ($Process.HasExited) { $Identity } else { "$($Process.Id):$($Process.StartTime.ToUniversalTime().Ticks)" }; exited = $Process.HasExited; exit_code = if ($Process.HasExited) { $Process.ExitCode } else { $null } }
        }.GetNewClosure()
        Signal = {
            $helper = Start-Process -FilePath (Join-Path $PSHOME 'pwsh.exe') -ArgumentList @(
                '-NoProfile', '-NonInteractive', '-File', ('"' + $signalScript + '"'),
                '-ProcessId', $Process.Id, '-StartTicks', $Process.StartTime.ToUniversalTime().Ticks
            ) -PassThru -WindowStyle Hidden
            try {
                if (-not $helper.WaitForExit(15000)) { $helper.Kill(); [void]$helper.WaitForExit(5000); return $false }
                return $helper.ExitCode -eq 0
            } finally {
                $helper.Dispose()
            }
        }.GetNewClosure()
        Wait = { param($ms) $Process.WaitForExit($ms) }.GetNewClosure()
        Kill = { $Process.Kill() }.GetNewClosure()
    }
}

function Stop-SoakChild([string]$ExpectedIdentity, [hashtable]$Adapter) {
    $outcome = [ordered]@{ stop_mode = 'unknown'; exit_confirmed = $false; signal_delivered = $false; exit_code = $null; errors = @() }
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $state = & $Adapter.State
        if ($state.identity -ne $ExpectedIdentity) { throw 'process_identity_mismatch' }
        if ($state.exited) {
            $outcome.exit_confirmed = $true
            $outcome.exit_code = $state.exit_code
            $outcome.stop_mode = "already_exited_$($state.exit_code)"
        } else {
            try { $outcome.signal_delivered = [bool](& $Adapter.Signal) }
            catch { $outcome.errors += 'signal_failed' }
            $gracefulExit = $false
            try { $gracefulExit = [bool](& $Adapter.Wait 60000) }
            catch { $outcome.errors += 'graceful_wait_failed' }
            if ($gracefulExit) {
                $state = & $Adapter.State
                if ($state.identity -ne $ExpectedIdentity -or -not $state.exited) { throw 'exit_identity_mismatch' }
                $outcome.exit_code = $state.exit_code
                $outcome.exit_confirmed = $true
                $outcome.stop_mode = "exited_$($state.exit_code)"
            } else {
                $state = & $Adapter.State
                if ($state.identity -ne $ExpectedIdentity) { throw 'kill_identity_mismatch' }
                if (-not $state.exited) { & $Adapter.Kill }
                $outcome.stop_mode = 'kill_unconfirmed'
                if (& $Adapter.Wait 30000) {
                    $state = & $Adapter.State
                    if ($state.identity -ne $ExpectedIdentity -or -not $state.exited) { throw 'kill_exit_identity_mismatch' }
                    $outcome.exit_confirmed = $true
                    $outcome.exit_code = $state.exit_code
                    $outcome.stop_mode = 'forced_kill'
                }
            }
        }
    } catch { $outcome.errors += 'process_cleanup_failed' }
    $outcome.shutdown_seconds = $watch.Elapsed.TotalSeconds
    return $outcome
}

function Complete-SoakEvidence($Result, [string]$StatusPath, [string]$ResultPath,
    [scriptblock]$Save = { param($v, $p) Save-SoakJson $v $p }) {
    $failures = 0
    $Result.publication_failures = @()
    $Result.finalization_complete = $false
    # Status failure must not suppress result publication (or vice versa).
    foreach ($path in @($StatusPath, $ResultPath)) {
        # The result is the last publication; it includes earlier status failure.
        $Result.finalization_complete = $path -eq $ResultPath -and $failures -eq 0
        try { & $Save $Result $path } catch {
            $failures++
            $Result.publication_failures += if ($path -eq $StatusPath) { 'status' } else { 'result' }
            $Result.finalization_complete = $false
        }
    }
    # If result publication fails, mark the earlier status incomplete if possible.
    if ($Result.publication_failures -contains 'result') {
        try { & $Save $Result $StatusPath } catch { }
    }
    return $failures
}
