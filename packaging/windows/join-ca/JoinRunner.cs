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
    static readonly Regex AddressChars = new Regex(@"\A[A-Za-z0-9.:/\[\]\-]{1,300}\z");

    public static string Run(string installFolder, string dataDir, string address, string token,
        Action<string> log, string extraArgs, int timeoutMs) {
      if (string.IsNullOrEmpty(token)) return FailedPrefix + "No join token was given.";
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
      string ps = Path.Combine(Environment.SystemDirectory, @"WindowsPowerShell\v1.0\powershell.exe");
      ProcessStartInfo psi = new ProcessStartInfo(ps);
      psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + folder + "\\join-worker.ps1\""
        + " -Exe \"" + folder + "\\lodestar.exe\""
        + " -Coordinator \"" + address + "\""
        + " -DataDir \"" + dataDir.TrimEnd('\\') + "\""
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
      DataReceivedEventHandler onLine = (sender, e) => {
        if (e.Data == null) return;
        lock (lines) { lines.Add(safe(e.Data)); }
      };
      using (Process p = new Process()) {
        p.StartInfo = psi;
        p.OutputDataReceived += onLine;
        p.ErrorDataReceived += onLine;
        p.Start();
        p.BeginOutputReadLine();
        p.BeginErrorReadLine();
        try {
          p.StandardInput.Write(token.Trim() + "\n");
          p.StandardInput.Flush();
        } catch (IOException) {
          // It exited before reading: its own output says why.
        } finally {
          try { p.StandardInput.Close(); } catch (IOException) { }
        }
        bool finished = p.WaitForExit(timeoutMs);
        if (finished) {
          p.WaitForExit();  // the output, read to its end
        } else {
          KillTree(p);
        }
        string failed = null;
        lock (lines) {
          foreach (string line in lines) {
            log(line);
            if (line.StartsWith(FailedPrefix, StringComparison.Ordinal)) failed = line;
          }
        }
        if (!finished) {
          log("join-worker.ps1 did not finish in time: stopped it");
          return FailedPrefix + "Joining " + address + " did not finish in time. Make a new join token and install again.";
        }
        if (p.ExitCode == 0) return null;
        return failed ?? (FailedPrefix + "Joining " + address + " failed (exit status " + p.ExitCode + "). The MSI log has the details.");
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
