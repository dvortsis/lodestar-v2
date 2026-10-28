param([Parameter(Mandatory)][int]$ProcessId, [Parameter(Mandatory)][long]$StartTicks)
$ErrorActionPreference = 'Stop'
try {
    $target = Get-Process -Id $ProcessId -ErrorAction Stop
    if ($target.HasExited -or $target.StartTime.ToUniversalTime().Ticks -ne $StartTicks) { exit 2 }
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class SoakSignal {
    delegate bool Handler(uint signal);
    static readonly ManualResetEvent Received = new ManualResetEvent(false);
    static readonly Handler Handle = signal => { Received.Set(); return true; };
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool AttachConsole(uint pid);
    [DllImport("kernel32.dll")] static extern bool FreeConsole();
    [DllImport("kernel32.dll")] static extern bool SetConsoleCtrlHandler(Handler handler, bool add);
    [DllImport("kernel32.dll")] static extern bool GenerateConsoleCtrlEvent(uint signal, uint group);
    public static bool Send(uint pid) {
        FreeConsole();
        if (!AttachConsole(pid)) return false;
        // This helper never detaches/restores handlers after sending. Its native
        // handler absorbs its own broadcast before the helper exits. The parent
        // supervisor is never attached to this console.
        if (!SetConsoleCtrlHandler(null, false) || !SetConsoleCtrlHandler(Handle, true)) return false;
        return GenerateConsoleCtrlEvent(0, 0) && Received.WaitOne(10000);
    }
}
'@
    # Recheck after compilation, immediately before attaching to the console.
    $target.Refresh()
    if ($target.HasExited -or $target.StartTime.ToUniversalTime().Ticks -ne $StartTicks) { exit 2 }
    if ([SoakSignal]::Send([uint32]$ProcessId)) { exit 0 }
    exit 3
} catch { exit 4 }
