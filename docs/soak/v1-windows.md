# Windows 1.0 soak: runbook

This is the owner's run for gate **G-v1-windows**. It tests the release candidate (RC) installer on your Windows PC, the way a normal user runs Lodestar: as a Windows service. It takes about **32 hours** (a restarted Lodestar pauses its crawler's sampling for up to 6 hours first), of which you are busy for about 40 minutes.

**Pass means these four hold** (the report's **Windows 1.0 checks** lists service restart, build binding, upgrade and uninstall; memory has its own section):

1. **Memory.** Private memory grows by no more than 2.0 MiB an hour over hours 12–24 (upper 95 % bound). This is CR-0's rule, unchanged.
2. **No service restart, same build throughout.** The service runs as one process for the whole 24 hours, and its program file (SHA-256) never changes during the run. The hash is the only proof of which build ran, because `lodestar --version` prints the same number for every 1.0 RC.
3. **Upgrade.** Upgrading from the last public release (`<last-release>`, below) leaves the service running on the new program file, with your data.
4. **Clean uninstall.** The service and program files are gone, and your data is kept or removed as you chose.

A *service* is a program Windows starts by itself and keeps running in the background. You do not need to sign in for it to run.

## Before you start

- Windows 10 or 11, an administrator account, Python 3.12 or newer, and [PowerShell 7](https://learn.microsoft.com/powershell/scripting/install/installing-powershell-on-windows) (`pwsh`).
- At least 50 GB free on `C:`. Docker Desktop stopped. Windows set not to sleep (Settings → System → Power), and **Windows Update paused for 5 weeks** (Settings → Windows Update → Pause), because a forced restart ends the run.
- Do not run the soak on a PC whose Lodestar data you want to keep. If it has one, first [back up its data](../user/03-keep-lodestar-safe.md#37-back-up-and-restore), or use another PC.
- Two installers: the MSI of the **last public release** before the RC (newest on the Releases page; v0.5.1 on 2026-11-12Z, so `<last-release>` below means `lodestar-0.5.1-x86_64.msi`) and the RC MSI. Check both as [section 4.2 of the user guide](../user/04-install-on-windows-and-linux.md#42-check-the-download-before-you-run-it) describes.
- A copy of this repository, for `docs/soak/tools`.
- Remove any earlier Lodestar first, so this run starts clean. This **deletes its data, and you can't undo it**: `Start-Process msiexec -ArgumentList '/x', '<old-msi>', 'REMOVE_DATA=1' -Wait`.

All commands run in an **elevated** PowerShell 7 (Start → `pwsh` → Run as administrator). Set these once. The run folder must not exist yet. Open PowerShell in the folder that holds both MSI files first (`cd` to it), because the `msiexec` commands use bare file names.

```powershell
$Tools = 'C:\path\to\repo\docs\soak\tools'
$Run   = 'C:\LodestarSoak\run'
$Life  = 'C:\LodestarSoak\lifecycle.json'
. "$Tools\cr0-common.ps1"
New-Item -ItemType Directory C:\LodestarSoak | Out-Null
```

## Phase 1: install the last release and let it fill (about 1 h)

1. Install `<last-release>` with the default options. Open `http://localhost:8090/`, finish first-run setup, and leave it crawling.
2. In Lodestar's settings, set **Intensity** to **Balanced**. The analyzer's limits assume Balanced.
3. Wait until the torrent count is above zero (about 30 minutes). Then record where you start, before the upgrade:

   ```powershell
   $Before  = Get-SoakTorrentCount '127.0.0.1:8090'
   $FromVer = Get-SoakInstalledVersion   # information only
   $OldPid  = Get-SoakServiceIdentity Lodestar
   $OldImg  = (Get-SoakServiceImage Lodestar).sha256
   ```

## Phase 2: upgrade to the RC (about 10 min)

1. Install the RC MSI over the top, default options. Do not uninstall first. Accept the administrator prompt. The command waits until the installer finishes:

   ```powershell
   Start-Process msiexec -ArgumentList '/i', 'lodestar-<rc>-x86_64.msi' -Wait
   ```
2. Wait until `Get-Service Lodestar` shows `Running` **and** `http://localhost:8090/` loads (the upgrade restarts the service; if the next command fails with a metrics error, wait a minute and run it again), then record the upgrade:

   ```powershell
   Save-SoakUpgrade $Life -FromVersion $FromVer -FromImageSha256 $OldImg -TorrentsBefore $Before -PreviousStartTicks $OldPid.start_ticks -Bind 127.0.0.1:8090
   ```

3. In Lodestar's settings (Settings → Updates), check that automatic updates are **Off**, which is the Windows default. An update installed during the soak would restart the service and fail the run. v0.5.1 has no update setting, so this check is after the upgrade.

   **Result:** `lifecycle.json` holds the two versions, the program file's hash before and after, whether the service runs, restarted and runs the installed program, whether the data folder is non-empty, and the torrent count before and after. **Pass needs** `torrents_after` at least `torrents_before`.

## Phase 3: the 24 h soak (about 31 h)

Start the collector. It watches the service and never stops or restarts it. It waits for the crawler's start-up pause to end, then a 30-minute warm-up, then records 24 hours.

```powershell
pwsh -File "$Tools\cr0-soak.ps1" -Exe 'C:\Program Files\Lodestar\lodestar.exe' -Root $Run `
  -Commit <rc-commit-sha> -Ui embedded -Bind 127.0.0.1:8090 -ServiceName Lodestar
```

Leave the window open and the PC awake. To check progress, read `$Run\status.json`. To stop early, create the file `$Run\stop.request`: the run then does not count.

**Result:** the window ends with `reason: baseline_window_complete` in `$Run\result.json`. Don't restart the service, Windows or the PC during the run. A restart ends the run early and the restart check fails; you start again from Phase 3.

## Phase 4: uninstall (about 5 min)

1. Keep the lifecycle file with the run, then uninstall and record it. Choose **kept** (the default uninstall) for this run:

   ```powershell
   Move-Item $Life "$Run\lifecycle.json"
   Start-Process msiexec -ArgumentList '/x', 'lodestar-<rc>-x86_64.msi' -Wait
   Save-SoakUninstall "$Run\lifecycle.json" -DataPolicy kept
   ```

   Use the same MSI file you installed. To also check data removal, add `'REMOVE_DATA=1'` to the `-ArgumentList` and pass `-DataPolicy removed`.

   **Result:** the service and `C:\Program Files\Lodestar` are gone; `C:\ProgramData\Lodestar` is still there (kept) or gone (removed).

## Phase 5: verdict (about 5 min)

```powershell
python "$Tools\analyze_cr0.py" $Run --output C:\LodestarSoak\report.md
```

Open `report.md`. The **Memory gate** says "meets the bar", and the **Windows 1.0 checks** section shows `pass` on all four lines: service restart, build binding, upgrade and uninstall. `absent` or `fail` on any of them is not a pass.

## What to send back

Send the runner **`report.md`** (the runner checks `service_image_sha256` in `run.json` against the RC's published checksums), plus `$Run\run.json`, `$Run\result.json` and `$Run\lifecycle.json`. They hold versions, hashes, counts, memory figures and pass/fail lines only. The time-zone field is withheld in service mode.

**Never send** the raw `samples.jsonl` or `metrics` folder unless asked, the database, anything under `C:\ProgramData\Lodestar` (logs, keys, the setup token), your password, or screenshots of the web page. Keep them private.
