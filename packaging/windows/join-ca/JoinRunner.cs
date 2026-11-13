// The core of the JoinWorker custom action, with no Windows Installer types so
// test-join-worker.ps1 can compile and drive it directly (JoinWorkerCA.cs is the
// thin MSI entry point around it).
//
// It starts join-worker.ps1 under Windows PowerShell, hands it the join token on
// standard input (never on a command line), relays its output with the token
// scrubbed out, and stops it at a deadline. It returns the "LODESTAR JOIN FAILED: "
// line for the user, or null when the join succeeded.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;

namespace Lodestar.Setup {
  public static class JoinRunner {
    public const string FailedPrefix = "LODESTAR JOIN FAILED: ";

    // join-worker.ps1 has its own 120 s deadline for lodestar; this one only
    // catches PowerShell itself hanging.
    public const int DefaultTimeoutMs = 300000;

    // Only what an address can hold: it goes on PowerShell's command line, and a
    // quote or a space there could add parameters. join-worker.ps1 checks it fully.
    // Starts with a letter, digit or [, so it can never read as a parameter name.
    static readonly Regex AddressChars = new Regex(@"\A[A-Za-z0-9\[][A-Za-z0-9.:/\[\]\-]{0,299}\z");

    // tempRoot: where join-worker.ps1 makes its folders (null: %SystemRoot%\Temp; tests
    // pass their own). This side names the folders, so it can delete the token folder
    // itself if it has to stop the script.
    public static string Run(string installFolder, string dataDir, string address, string token,
        Action<string> log, string tempRoot, string extraArgs, int timeoutMs) {
      if (string.IsNullOrWhiteSpace(token)) return FailedPrefix + "No join token was given.";
      address = (address ?? "").Trim();
      if (!AddressChars.IsMatch(address)) {
        return FailedPrefix + "The main Lodestar's address should look like https://192.168.1.10:8091.";
      }
      foreach (string p in new[] { installFolder, dataDir }) {
        if (string.IsNullOrEmpty(p) || p.IndexOf('"') >= 0) return FailedPrefix + "Unexpected install folder.";
      }
      Regex scrub = new Regex(Regex.Escape(token.Trim()), RegexOptions.IgnoreCase);
      Func<string, string> safe = s => s == null ? "" : scrub.Replace(s, "[join token]");

      string folder = installFolder.TrimEnd('\\');
      string root = tempRoot ?? Path.Combine(Environment.GetEnvironmentVariable("SystemRoot") ?? @"C:\Windows", "Temp");
      if (root.IndexOf('"') >= 0) return FailedPrefix + "Unexpected temporary folder.";
      string id = Guid.NewGuid().ToString("N");
      string tokenDir = Path.Combine(Path.Combine(root, "lodestar-join-" + id), "t-" + id);
      string ps = Path.Combine(Environment.SystemDirectory, @"WindowsPowerShell\v1.0\powershell.exe");
      ProcessStartInfo psi = new ProcessStartInfo(ps);
      psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + folder + "\\join-worker.ps1\""
        + " -Exe \"" + folder + "\\lodestar.exe\""
        + " -Coordinator \"" + address + "\""
        + " -DataDir \"" + dataDir.TrimEnd('\\') + "\""
        + " -WorkId " + id
        + (tempRoot == null ? "" : " -TempRoot \"" + root.TrimEnd('\\') + "\"")
        + (string.IsNullOrEmpty(extraArgs) ? "" : " " + extraArgs);
      psi.UseShellExecute = false;
      psi.CreateNoWindow = true;
      psi.RedirectStandardInput = true;
      psi.RedirectStandardOutput = true;
      psi.RedirectStandardError = true;
      psi.StandardOutputEncoding = Encoding.UTF8;
      psi.StandardErrorEncoding = Encoding.UTF8;
      // Only stdin carries the token: nothing inherited from msiexec's environment joins.
      foreach (string k in new[] { "LODESTAR_TOKEN", "LODESTAR_JOIN", "LODESTAR_TOKEN_IGNORED" }) {
        psi.EnvironmentVariables.Remove(k);
      }

      // Collected on the reader threads, logged on this one afterwards: the MSI
      // session is only used from the custom action's own thread.
      List<string> lines = new List<string>();
      ManualResetEvent outDone = new ManualResetEvent(false), errDone = new ManualResetEvent(false);
      Func<ManualResetEvent, DataReceivedEventHandler> reader = done => (sender, e) => {
        if (e.Data == null) { done.Set(); return; }
        lock (lines) { lines.Add(safe(e.Data)); }
      };
      using (Process p = new Process()) {
        p.StartInfo = psi;
        p.OutputDataReceived += reader(outDone);
        p.ErrorDataReceived += reader(errDone);
        p.Start();
        p.BeginOutputReadLine();
        p.BeginErrorReadLine();
        try {
          // Raw ASCII bytes on the pipe itself: the StreamWriter would put a UTF-8
          // byte-order mark first when the console's encoding is UTF-8.
          byte[] bytes = Encoding.ASCII.GetBytes(token.Trim() + "\n");
          try {
            p.StandardInput.BaseStream.Write(bytes, 0, bytes.Length);
            p.StandardInput.BaseStream.Flush();
          } finally {
            Array.Clear(bytes, 0, bytes.Length);
          }
        } catch (IOException) {
          // It exited before reading: its own output says why.
        } finally {
          try { p.StandardInput.Close(); } catch (IOException) { }
        }
        bool finished = p.WaitForExit(timeoutMs);
        if (!finished) KillTree(p);
        // The rest of the output, with a deadline: a child that kept the pipes open
        // can't hold the install.
        // One handle at a time against one deadline: WaitAll on several handles
        // throws on a single-threaded-apartment thread, such as PowerShell's.
        DateTime outputDeadline = DateTime.UtcNow.AddSeconds(30);
        foreach (ManualResetEvent done in new[] { outDone, errDone }) {
          TimeSpan left = outputDeadline - DateTime.UtcNow;
          done.WaitOne(left > TimeSpan.Zero ? left : TimeSpan.Zero);
        }
        // Stopped, the script's own finally never ran: delete the token folder here.
        string leftover = null;
        if (!finished && !RemoveFolder(tokenDir)) leftover = tokenDir;
        string failed = null;
        lock (lines) {
          foreach (string line in lines) {
            log(line);
            if (line.StartsWith(FailedPrefix, StringComparison.Ordinal)) failed = line;
          }
        }
        if (leftover != null) {
          return FailedPrefix + "The temporary token file in " + leftover + " could not be deleted. Delete that folder as an administrator.";
        }
        if (!finished) {
          log("join-worker.ps1 did not finish in time: stopped it");
          return FailedPrefix + "Joining " + address + " did not finish in time. Make a new join token and install again.";
        }
        if (p.ExitCode == 0) return null;
        return failed ?? (FailedPrefix + "Joining " + address + " failed (exit status " + p.ExitCode + "). The MSI log has the details.");
      }
    }

    // Deletes dir, retrying for up to 10 s while a stopped process still holds a file in
    // it. True once it's gone.
    static bool RemoveFolder(string dir) {
      DateTime deadline = DateTime.UtcNow.AddSeconds(10);
      while (true) {
        try { if (Directory.Exists(dir)) Directory.Delete(dir, true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
        if (!Directory.Exists(dir)) return true;
        if (DateTime.UtcNow >= deadline) return false;
        Thread.Sleep(200);
      }
    }

    static void KillTree(Process p) {
      try {
        string taskkill = Path.Combine(Environment.SystemDirectory, "taskkill.exe");
        ProcessStartInfo k = new ProcessStartInfo(taskkill, "/PID " + p.Id + " /T /F");
        k.UseShellExecute = false;
        k.CreateNoWindow = true;
        using (Process t = Process.Start(k)) { t.WaitForExit(30000); }
      } catch (Exception) { }
      try { if (!p.HasExited) p.Kill(); } catch (Exception) { }
      p.WaitForExit(30000);
    }
  }
}
