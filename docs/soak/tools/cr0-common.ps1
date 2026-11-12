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
    },
    [bool]$Attach = $false) {
    $uri = $null
    if (-not [Uri]::TryCreate("http://$Bind", [UriKind]::Absolute, [ref]$uri) -or
        $uri.Host -ne '127.0.0.1' -or $uri.Port -lt 1024 -or $uri.Port -gt 65535 -or
        $uri.AbsolutePath -ne '/' -or $uri.Query -or $uri.Fragment -or $uri.UserInfo) {
        throw 'Use an explicit unprivileged IPv4 loopback HTTP bind.'
    }
    $names = @(& $Processes)
    # Service mode attaches to the installed service, which owns the process and ports.
    if (-not $Attach -and ($names | Where-Object { $_ -like 'lodestar*' })) { throw 'Another Lodestar process is running.' }
    if ($names | Where-Object { $_ -in @('Docker Desktop', 'com.docker.backend', 'dockerd', 'docker-proxy') }) {
        throw 'Docker must remain stopped for the native baseline.'
    }
    if ($Attach) { return }
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

# ---- Windows 1.0 soak: service identity and lifecycle records (M8-WIN-1) ----
# Aggregate-only: versions, booleans and counts. No paths, user names or addresses.
function Get-SoakServiceIdentity([string]$Name,
    [scriptblock]$Query = { param($n) $svc = Get-CimInstance Win32_Service -Filter "Name='$n'"
        if ($svc -and $svc.ProcessId) { $proc = Get-Process -Id $svc.ProcessId
            @{ pid = [int]$svc.ProcessId; start_ticks = $proc.StartTime.ToUniversalTime().Ticks } } }) {
    $found = & $Query $Name
    if ($null -eq $found) { throw 'Service is not running.' }
    return $found
}

function Get-SoakImagePath([string]$PathName) {
    # The executable in a service's ImagePath: quoted, or up to the first ".exe".
    if ($PathName -match '^\s*"([^"]+)"') { return $Matches[1] }
    if ($PathName -match '^\s*(.+?\.exe)(\s|$)') { return $Matches[1] }
    throw 'Cannot read the service image path.'
}

function Get-SoakServiceImage([string]$Name,
    [scriptblock]$Query = { param($n) (Get-CimInstance Win32_Service -Filter "Name='$n'").PathName }) {
    # SHA-256 of the binary the service is registered to run; the path is not recorded.
    # lodestar.exe has no file-version resource, so the hash is the only binding.
    $path = Get-SoakImagePath (& $Query $Name)
    @{ sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
}

function Assert-SoakAttachBinding([string]$Name, [string]$Exe, [hashtable]$Probe = @{}) {
    # At attach, the file the soak hashes must be the file the service process runs.
    # An MSI upgrade can replace the file and leave the old process running (files in use,
    # reboot pending). Refuse then, so run.sha256 and the samples describe the process measured.
    # Only hashes are compared and reported; no path leaves this function. $Probe replaces the Windows calls in tests.
    $p = @{
        Identity = { param($n) Get-SoakServiceIdentity $n }
        ProcessImage = { param($procId) (Get-Process -Id $procId).Path }
        Hash = { param($f) (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash }
        ServiceImage = { param($n) (Get-SoakServiceImage $n).sha256 }
    }
    foreach ($key in $Probe.Keys) { $p[$key] = $Probe[$key] }
    $registered = & $p.ServiceImage $Name
    $running = & $p.ProcessImage (& $p.Identity $Name).pid
    if ([string]::IsNullOrEmpty($running)) { throw 'Cannot read the running service program file. Run this from an elevated PowerShell.' }
    if ((& $p.Hash $running) -ne $registered) {
        throw 'The running service is not the installed program file (an upgrade may be waiting on a restart). Restart the service, or reboot if Windows says a restart is pending, then start the soak again.'
    }
    if ((& $p.Hash $Exe) -ne $registered) {
        throw '-Exe is not the file the service is installed from. Point -Exe at the installed lodestar.exe.'
    }
}

function Save-SoakLifecycle([string]$Path, [string]$Step, [hashtable]$Fields) {
    # Merge one step ('upgrade' or 'uninstall') into lifecycle.json.
    $record = if ([IO.File]::Exists($Path)) { Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json -AsHashtable } else { @{ schema_version = 1 } }
    $record[$Step] = $Fields
    Save-SoakJson $record $Path
}

function Get-SoakTorrentCount([string]$Bind,
    [scriptblock]$Fetch = { param($url) (Invoke-WebRequest $url -TimeoutSec 5).Content }) {
    $text = & $Fetch "http://$Bind/metrics"
    $match = [regex]::Match($text, '(?m)^lodestar_torrents ([0-9.]+)\r?$')
    if (-not $match.Success) { throw 'Missing metric: torrents' }
    return [long][double]::Parse($match.Groups[1].Value, [Globalization.CultureInfo]::InvariantCulture)
}

function Get-SoakInstalledVersion([string]$Exe = (Join-Path $env:ProgramFiles 'Lodestar\lodestar.exe'),
    [scriptblock]$Version = { param($path) Get-SoakExecutableVersion $path }) {
    # Information only: --version prints Cargo.toml's base version, so two RCs look alike.
    (& $Version $Exe) -replace '^\S+\s+', ''
}

function Save-SoakUpgrade([Parameter(Mandatory)][string]$Path, [string]$FromVersion,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{64}$')][string]$FromImageSha256,
    [Parameter(Mandatory)][ValidateRange(1, [long]::MaxValue)][long]$TorrentsBefore,
    [Parameter(Mandatory)][ValidateRange(1, [long]::MaxValue)][long]$PreviousStartTicks,
    [string]$Bind = '127.0.0.1:8090', [string]$Service = 'Lodestar', [string]$DataDir = (Join-Path $env:ProgramData 'Lodestar'),
    [string]$Exe = (Join-Path $env:ProgramFiles 'Lodestar\lodestar.exe'), [hashtable]$Probe = @{}) {
    # Run after the MSI upgrade finished. FromVersion, FromImageSha256 and PreviousStartTicks
    # are measured before it. $Probe replaces the Windows calls in tests.
    $p = @{
        Identity = { param($n) Get-SoakServiceIdentity $n }
        Status = { param($n) (Get-Service $n).Status }
        ProcessImage = { param($procId) (Get-Process -Id $procId).Path }
        Hash = { param($f) (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash }
        Data = { param($d) [bool](Get-ChildItem -LiteralPath $d -ErrorAction SilentlyContinue | Select-Object -First 1) }
        Torrents = { param($b) Get-SoakTorrentCount $b }
        Version = { param($e) Get-SoakInstalledVersion $e }
        ServiceImage = { param($n) (Get-SoakServiceImage $n).sha256 }
    }
    foreach ($key in $Probe.Keys) { $p[$key] = $Probe[$key] }
    $now = & $p.Identity $Service
    $image = & $p.ProcessImage $now.pid
    Save-SoakLifecycle $Path 'upgrade' @{
        from_version = $FromVersion; to_version = (& $p.Version $Exe)
        from_image_sha256 = $FromImageSha256; to_image_sha256 = (& $p.ServiceImage $Service)
        service_running = ((& $p.Status $Service) -eq 'Running')
        service_restarted = ($now.start_ticks -gt $PreviousStartTicks)
        image_matches_installed = ((& $p.Hash $image) -eq (& $p.Hash $Exe))
        data_present = [bool](& $p.Data $DataDir)
        torrents_before = $TorrentsBefore; torrents_after = (& $p.Torrents $Bind)
    }
}

function Save-SoakUninstall([string]$Path, [ValidateSet('kept', 'removed')][string]$DataPolicy,
    [string]$Service = 'Lodestar', [string]$DataDir = (Join-Path $env:ProgramData 'Lodestar'),
    [string]$ProgramDir = (Join-Path $env:ProgramFiles 'Lodestar'), [hashtable]$Probe = @{}) {
    $p = @{
        ServicePresent = { param($n) [bool](Get-Service $n -ErrorAction SilentlyContinue) }
        Exists = { param($d) [bool](Test-Path -LiteralPath $d) }
    }
    foreach ($key in $Probe.Keys) { $p[$key] = $Probe[$key] }
    Save-SoakLifecycle $Path 'uninstall' @{
        service_present = (& $p.ServicePresent $Service)
        program_files_present = (& $p.Exists $ProgramDir)
        data_present = (& $p.Exists $DataDir); data_policy = $DataPolicy
    }
}
