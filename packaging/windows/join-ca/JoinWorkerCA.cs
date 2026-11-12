// The JoinWorker custom action (join.wxs): deferred, run as SYSTEM, once, on a
// first install in "Join an existing Lodestar" mode. Its CustomActionData holds
// the token; the action that sets it and this one are HideTarget, and the
// property is in MsiHiddenProperties, so the MSI log never shows it. All the work
// is in JoinRunner and join-worker.ps1.
using System;
using System.IO;
using Microsoft.Deployment.WindowsInstaller;

namespace Lodestar.Setup {
  public static class JoinWorkerCA {
    [CustomAction]
    public static ActionResult JoinWorker(Session session) {
      string failed;
      try {
        CustomActionData data = session.CustomActionData;
        // The folders are never taken from the data: only the ones main.wxs allows
        // (WrongInstallFolder refuses any other install folder), so a crafted address
        // can't point SYSTEM at a script elsewhere.
        string install = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "Lodestar");
        string dataDir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData), "Lodestar");
        string given = data.ContainsKey("INSTALLFOLDER") ? data["INSTALLFOLDER"] : "";
        if (!string.Equals(given.TrimEnd('\\'), install, StringComparison.OrdinalIgnoreCase)) {
          failed = JoinRunner.FailedPrefix + "Lodestar installs to " + install + " only.";
        } else {
          failed = JoinRunner.Run(install, dataDir,
            data.ContainsKey("ADDRESS") ? data["ADDRESS"] : null,
            data.ContainsKey("TOKEN") ? data["TOKEN"] : null,
            line => session.Log("JoinWorker: " + line),
            null, null, JoinRunner.DefaultTimeoutMs);
        }
      } catch (Exception e) {
        // A malformed action data (a ; or = in the address) ends here. The message is the
        // exception's type only: its text could quote the data.
        failed = JoinRunner.FailedPrefix + "The address could not be read (" + e.GetType().Name + "). Use one like https://192.168.1.10:8091.";
      }
      if (failed == null) return ActionResult.Success;
      // Field 1 is shown as it is: a [bracketed] address is not read as a property.
      using (Record r = new Record(1)) {
        r.FormatString = "[1]";
        r[1] = failed.Substring(JoinRunner.FailedPrefix.Length);
        // MB_OK | MB_ICONERROR: one OK button. Shown in a wizard install, logged in any.
        session.Message(InstallMessage.Error | (InstallMessage)0x10, r);
      }
      return ActionResult.Failure;
    }
  }
}
