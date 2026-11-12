// The JoinWorker custom action (join.wxs): deferred, run as SYSTEM, once, on a
// first install in "Join an existing Lodestar" mode. Its CustomActionData holds
// the token; the action that sets it and this one are HideTarget, and the
// property is in MsiHiddenProperties, so the MSI log never shows it. All the work
// is in JoinRunner and join-worker.ps1.
using Microsoft.Deployment.WindowsInstaller;

namespace Lodestar.Setup {
  public static class JoinWorkerCA {
    [CustomAction]
    public static ActionResult JoinWorker(Session session) {
      CustomActionData data = session.CustomActionData;
      string failed = JoinRunner.Run(
        data.ContainsKey("INSTALLFOLDER") ? data["INSTALLFOLDER"] : null,
        data.ContainsKey("DATADIR") ? data["DATADIR"] : null,
        data.ContainsKey("ADDRESS") ? data["ADDRESS"] : null,
        data.ContainsKey("TOKEN") ? data["TOKEN"] : null,
        line => session.Log("JoinWorker: " + line),
        null,
        JoinRunner.DefaultTimeoutMs);
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
