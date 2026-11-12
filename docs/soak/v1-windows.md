# Windows 1.0 soak: runbook

This is the owner's run for gate **G-v1-windows**. It tests the release candidate (RC) installer on your Windows PC, the way a normal user runs Lodestar: as a Windows service. It takes about **26 hours**, of which you are busy for about 40 minutes.

**Pass means all four hold:**

1. **Memory.** Private memory grows by no more than 2.0 MiB an hour over hours 12–24 (upper 95 % bound). This is CR-0's rule, unchanged.
2. **No service restart.** The service runs as one process for the whole 24 hours.
3. **Upgrade.** Upgrading from the last public release (v0.5.1) leaves the service running on the new version, with your data.
4. **Clean uninstall.** The service and program files are gone, and your data is kept or removed as you chose.

A *service* is a program Windows starts by itself and keeps running in the background. You do not need to sign in for it to run.

## Before you start

- Windows 10 or 11, an administrator account, Python 3.12 or newer, and [PowerShell 7](https://learn.microsoft.com/powershell/scripting/install/installing-powershell-on-windows) (`pwsh`).
- At least 50 GB free on `C:`. Docker Desktop stopped. Windows set not to sleep (Settings → System → Power).
- Two installers: the **v0.5.1** MSI (`lodestar-0.5.1-x86_64.msi`) and the RC MSI. Check both as [section 4.2 of the user guide](../user/04-install-on-windows-and-linux.md#42-check-the-download-before-you-run-it) describes.
- A copy of this repository, for `docs/soak/tools`.
- Remove any earlier Lodestar first, with its data (`REMOVE_DATA=1`), so this run starts clean.

All commands run in an **elevated** PowerShell 7 (Start → `pwsh` → Run as administrator). Set these once. The run folder must not exist yet.

```powershell
$Tools = 'C:\path\to\repo\docs\soak\tools'
$Run   = 'C:\LodestarSoak\run'
$Life  = 'C:\LodestarSoak\lifecycle.json'
. "$Tools\cr0-common.ps1"
New-Item -ItemType Directory C:\LodestarSoak | Out-Null
```

## Phase 1: install v0.5.1 and let it fill (about 1 h)

1. Install `lodestar-0.5.1-x86_64.msi` with the default options. Open `http://localhost:8090/`, finish first-run setup, and leave it crawling.
2. Wait until the torrent count is above zero (about 30 minutes). Then write it down:

   ```powershell
   $Before = Get-SoakTorrentCount '127.0.0.1:8090'; $Before
   ```

## Phase 2: upgrade to the RC (about 10 min)

1. Install the RC MSI over the top: `msiexec /i lodestar-<rc>-x86_64.msi`, default options. Do not uninstall first. Accept the administrator prompt.
2. Wait for `Get-Service Lodestar` to show `Running`, then record the upgrade:

   ```powershell
   Save-SoakUpgrade $Life -FromVersion 0.5.1 -TorrentsBefore $Before -Bind 127.0.0.1:8090
   ```

   **Result:** `lifecycle.json` holds the two versions, whether the service runs, whether the data folder is non-empty, and the torrent count before and after. **Pass needs** `torrents_after` at least `torrents_before`.

## Phase 3: the 24 h soak (about 25 h)

Start the collector. It watches the service and never stops or restarts it. It waits for the crawler's start-up pause to end, then a 30-minute warm-up, then records 24 hours.

```powershell
pwsh -File "$Tools\cr0-soak.ps1" -Exe 'C:\Program Files\Lodestar\lodestar.exe' -Root $Run `
  -Commit <rc-commit-sha> -Ui embedded -Bind 127.0.0.1:8090 -ServiceName Lodestar
```

Leave the window open and the PC awake. To check progress, read `$Run\status.json`. To stop early, create the file `$Run\stop.request`: the run then does not count.

**Result:** the window ends with `reason: baseline_window_complete` in `$Run\result.json`. Don't restart the service, Windows or the PC during the run; a restart fails the run, and you start again.

## Phase 4: uninstall (about 5 min)

1. Keep the lifecycle file with the run, then uninstall and record it. Choose **kept** (the default uninstall) for this run:

   ```powershell
   Move-Item $Life "$Run\lifecycle.json"
   msiexec /x lodestar-<rc>-x86_64.msi
   Save-SoakUninstall "$Run\lifecycle.json" -DataPolicy kept
   ```

   Use the same MSI file you installed. To also check data removal, add `REMOVE_DATA=1` to the `msiexec` line and pass `-DataPolicy removed`.

   **Result:** the service and `C:\Program Files\Lodestar` are gone; `C:\ProgramData\Lodestar` is still there (kept) or gone (removed).

## Phase 5: verdict (about 5 min)

```powershell
python "$Tools\analyze_cr0.py" $Run --output C:\LodestarSoak\report.md
```

Open `report.md`. The **Memory gate** says "meets the bar", and the **Windows 1.0 checks** section shows `pass` for all three. `absent` or `fail` on any of them is not a pass.

## What to send back

Send the runner **`report.md`**, plus `$Run\run.json`, `$Run\result.json` and `$Run\lifecycle.json`. They hold versions, counts, memory figures and pass/fail lines only.

**Never send** the raw `samples.jsonl` or `metrics` folder unless asked, the database, anything under `C:\ProgramData\Lodestar` (logs, keys, the setup token), your password, or screenshots of the web page. Keep them private.
