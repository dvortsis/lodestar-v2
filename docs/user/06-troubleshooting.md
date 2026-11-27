# Chapter 6: Troubleshooting

This chapter lists common problems with fixes, then says how to get help.

## 6.1 Common problems

| Problem | Likely cause | Fix |
|---|---|---|
| The web page doesn't load | The [container](glossary.md#container) isn't running, or the port is taken | Check that the container shows as started. On Unraid, open the Docker tab. With Compose, run `docker compose ps` on the Docker host. If another app uses `8090`, change the host side of the **Web UI** port. |
| Lodestar exits with "started as root without the SETUID/SETGID capabilities" | The container was started with those permissions removed | In Compose, remove `cap_drop: ALL`, or add `cap_add: [SETUID, SETGID, CHOWN]` after it ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| Lodestar exits saying the data folder "doesn't look like Lodestar's" | **Data** holds other files and belongs to someone else | Point **Data** at an empty folder, such as `/mnt/cache/appdata/lodestar` ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| **Apply** or `docker compose up` fails with "manifest unknown", or with `failed to resolve reference "…": …: not found` | The image name or tag is wrong. The name is `lodestar-v2`, not `lodestar`, and the tag has no `v`: `0.5.1`, not `v0.5.1` | Set the repository or Compose image to `ghcr.io/dvortsis/lodestar-v2:0.5.1` exactly, then pull it again. `0.5` and `latest` also work. Don't use `nightly`: it's a development build that can leave data v0.5.1 can't read. |
| You lost the [setup token](glossary.md#setup-token) | The log scrolled, or you haven't created a password yet | Restart the container. Read the log again and use the newest token. |
| Few new torrents appear after a restart | v0.5.1: sampling waits six hours after each start. **(Newer than v0.5.1)** There is no wait. Sampling starts once Lodestar has found its first [DHT](glossary.md#dht) nodes, so a slow start means it is still finding nodes or the DHT port is blocked | v0.5.1: leave Lodestar running. Full speed starts after those six hours. Search remains available. Newer: wait a few minutes, then check the connection chapter's DHT steps ([section 3.3](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn)). The index grows over days and weeks. |
| Nothing is discovered | The [DHT](glossary.md#dht) can't bootstrap, or outbound [UDP](glossary.md#udp) is blocked | Read the log for DHT errors. Check that DNS and outbound UDP work from the server. Without a forwarded port the crawler is slow but still works. |
| The database is slow or reports "database is locked" | The data folder is under `/mnt/user` or on a network share | Move the data folder to a pool path such as `/mnt/cache/appdata/lodestar` ([section 2.2](02-install-lodestar-on-unraid-and-docker.md#22-settings-in-the-unraid-template)). |
| Your home network gets slow | The crawler fills the router's connection table | Set **Intensity** to `low` and raise it step by step. |
| The web UI won't load and nothing new is found after gluetun restarts or updates | Lodestar shares gluetun's network. When gluetun's container restarts, Lodestar stays attached to the old network, which no longer works, while Docker still shows it as running. To check, run `sudo readlink /proc/$(docker inspect -f '{{.State.Pid}}' gluetun)/ns/net /proc/$(docker inspect -f '{{.State.Pid}}' lodestar)/ns/net` on the server: two different lines mean Lodestar is on the old network | Restart Lodestar: on Unraid, select its icon on the Docker tab, then **Restart**. With Compose, run `docker compose restart lodestar`, or `docker compose up -d --force-recreate lodestar` if another tool updated gluetun. In v0.5.1 that restart costs the six-hour sampling wait (search keeps working); **(Newer than v0.5.1)** it doesn't. **(Newer than v0.5.1)** Lodestar's gluetun templates set `LODESTAR_EXIT_ON_NETWORK_LOSS=true` and a restart policy, and Lodestar then restarts itself within about half a minute. If yours doesn't: on Unraid, add the Variable `LODESTAR_EXIT_ON_NETWORK_LOSS` with value `true`, and add `--restart=unless-stopped` to **Extra Parameters**. With Compose, add the variable and `restart: true` under `depends_on: gluetun:` ([section 3.3](03-keep-lodestar-safe.md#run-lodestar-through-gluetun-with-docker-compose)). If Lodestar shows as stopped after gluetun was down for a while, start Lodestar again: Docker doesn't retry a start that fails because gluetun isn't running. Companions that use gluetun's network, such as qBittorrent, still need a restart by hand after gluetun restarts outside Compose. |
| The Windows service won't start and Windows shows "service-specific error 536870915" (older versions: "error 3") | The data folder `C:\ProgramData\Lodestar` isn't private any more, most often because **Continue** was selected on its permission prompt in File Explorer | Follow [section 6.2](#62-fix-windows-service-error-3). |
| Windows logs three failure events for one problem | Windows restarts the service twice after it stops with an error, 60 seconds apart, so one problem shows three times. **(Newer than v0.5.1)** When the cause is the data folder ([section 6.2](#62-fix-windows-service-error-3)), the Application log also has three notes from the source **Lodestar**, one for each start | Fix the cause once, as in [section 6.2](#62-fix-windows-service-error-3). The count starts again after a day. |
| Other computers can't open the web page, or few torrents are found, on Windows | Windows treats your network as a **Public network**. Lodestar's firewall rules allow private networks only, on purpose | If this is your own home network and you trust every device on it, set it to private: **Settings → Network & internet**, select your connection, then under **Network profile type** select **Private network**. On a network you don't control, such as a café or hotel, leave it **Public**. Never add the Public profile to Lodestar's rules: that opens Lodestar to everyone on such networks. |
| Lodestar logs the warning `logging.folder_shared` on Windows | The `logs` folder has an extra account in its permissions | In an [elevated PowerShell](glossary.md#elevated-powershell), run `icacls "C:\ProgramData\Lodestar\logs"` to see the accounts. SYSTEM, Administrators, `NT SERVICE\Lodestar`, `OWNER RIGHTS` and `CREATOR OWNER` belong there. Remove any other account with `icacls "C:\ProgramData\Lodestar\logs" /remove:g "<account>" /T /C`, then run `Restart-Service Lodestar`. |
| The Windows service stops at once and stays stopped | Windows restarts it twice after an error, then leaves it stopped until the count resets after a day | **(Newer than v0.5.1)** First open **Event Viewer**, then **Windows Logs → Application**, and look for an **Error** from the source **Lodestar** (event ID 3, 5 or 6). If there is one, follow [section 6.2](#62-fix-windows-service-error-3). Otherwise look for the reason in the log. In an [elevated PowerShell](glossary.md#elevated-powershell), run `Get-Content -LiteralPath 'C:\ProgramData\Lodestar\logs\lodestar.log' -Tail 50`. If Windows reported error 536870915 or "error 3", follow [section 6.2](#62-fix-windows-service-error-3). Otherwise fix what the log names, then run `Start-Service Lodestar`. |
| Lodestar won't start and the web page doesn't load. On Linux the journal says `serve: binding 0.0.0.0:8090: Address already in use`. The portable ZIP's console says `binding 0.0.0.0:8090` and "Only one usage of each socket address". The Windows service log says only `service: stopped on an error` | Another program already uses port `8090`, so Lodestar can't start | On Windows, run `netstat -ano \| findstr :8090` in an elevated PowerShell. Take the number at the end of the line and run `Get-Process -Id <number>` to see which program it is. Stop or reconfigure that program. On Linux, run `sudo ss -ltnp \| grep 8090`. To move Lodestar instead on Linux, run `sudo systemctl edit lodestar`, add `[Service]` and `Environment=LODESTAR_HTTP_BIND=0.0.0.0:8091`, then run `sudo systemctl restart lodestar` and open `http://localhost:8091/` from then on. |
| The wizard, Settings or Insights warns about the data folder, or about ports on Windows | The data folder is on a [FUSE](glossary.md#fuse) or network filesystem, or Windows has too few outgoing ports left | See [section 4.6](04-install-on-windows-and-linux.md#46-fix-a-data-folder-warning) and [section 4.7](04-install-on-windows-and-linux.md#47-fix-the-windows-port-warning). |

## 6.2 Fix Windows service error 3
<!-- index: error 3; error 536870915; 0x20000003; event 7024; event ID 5; event ID 6; data folder is a file; can't be read; can't be written; can't be created; full path; Windows service; icacls; Repair; folder_shared; service-specific error 3 -->

If Event Viewer shows event ID 5 or 6 from **Lodestar**, go to "Other refusals" at the end of this section.

At every start the Lodestar service checks that `C:\ProgramData\Lodestar` is private: only SYSTEM, Administrators and `NT SERVICE\Lodestar` have permissions, nothing is inherited, and nothing inside is a link. If the check fails, the service refuses to start. Windows shows "service-specific error 536870915" (`0x20000003`). Event Viewer has event 7024 in the **System** log, which may show only the raw code, such as `%%536870915`. Older versions show "error 3" instead.

**(Newer than v0.5.1)** Lodestar also writes a short note to the Application log. Open **Event Viewer**, then **Windows Logs → Application**, and look for an **Error** from the source **Lodestar**, event ID 3.

It names the data folder and says to run the installer's **Repair** as an administrator. It holds only the folder's path and that fix, so anyone who can open Event Viewer can read it. The exact reason is still in the file below.

Windows doesn't know why. Lodestar writes the reason to `C:\Windows\Temp\lodestar-service.log`, which only administrators can read. It names the folder and the extra account. When the one problem is a single extra user account, it also gives the exact command that removes it.

> [!WARNING]
> Don't delete the folder: it holds your library, your password and your keys. The steps below remove only the extra account.

**Recommended:** if the extra account is your own administrator account, run the installer's **Repair** (step 2). It removes your account's entries and makes the folder private again. It refuses anything else, so it can't hide another problem.

To fix it:

1. Read the reason. Open an [elevated PowerShell](glossary.md#elevated-powershell) ([section 4.4](04-install-on-windows-and-linux.md#44-install-on-windows)) and run:

    ```powershell
    Get-Content "C:\Windows\Temp\lodestar-service.log" -Tail 20
    ```

   **Result:** The last line says the folder isn't private and why, for example that it grants access to `DESKTOP-1\alex (S-1-5-21-…)`.
2. If the account it names is your own, run **Repair**. Open **Control Panel → Programs and Features**, right-click **Lodestar**, then select **Repair**. Allow it when Windows asks for administrator permission.

   Repair can use your account only if it is listed directly in `net localgroup Administrators`. A work or school (Microsoft Entra ID) account can't use it: `whoami /user` shows its number starting with `S-1-12-1-`. Run Repair anyway: it checks the whole folder first and tells you what to do.

   **Result:** Repair finishes without an error and starts the service. Continue with step 4. If Repair stops with an error, read why in its log. This finds only logs that Lodestar's installer made:

    ```powershell
    $owners = "S-1-5-18", "S-1-5-32-544"
    $owner = { param($p) (Get-Acl -LiteralPath $p).GetOwner([Security.Principal.SecurityIdentifier]).Value }
    $log = Get-ChildItem "C:\Windows\Temp\lodestar-secure-*\secure-datadir.log" |
      Where-Object { -not (($_.Attributes -bor $_.Directory.Attributes) -band [IO.FileAttributes]::ReparsePoint) } |
      Where-Object { (& $owner $_.DirectoryName) -in $owners -and (& $owner $_.FullName) -in $owners } |
      Sort-Object LastWriteTime | Select-Object -Last 1
    if ($log) { Get-Content $log.FullName -Tail 3 } else { "No installer log found" }
    ```

   If the last line says `STOPPED: Repair can't fix this for your account`, continue with step 3. If it says anything else, Repair found another problem: don't run step 3, and read the last paragraph of this section.
3. Remove your own account's entries. Don't copy a command from a log: these lines work out your account's number themselves, so they never remove anyone else's.

    ```powershell
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    icacls "$env:ProgramData\Lodestar" /remove:g "*$me" /T /C /L
    ```

   Compare `$me` with the account number in step 1's reason. If they differ, for example because you opened PowerShell as another administrator, ask for help ([section 6.3](#63-get-help)).

   **Result:** `icacls` reports the files it processed. It removes your account's entries and changes nothing else.
4. Start the service, wait about 10 seconds, then check that it is still running:

    ```powershell
    Start-Service Lodestar
    Get-Service Lodestar
    ```

   **Result:** `Status` shows `Running`, and the web page loads. If it shows `Stopped`, the check refused again: repeat step 1 and read the new reason.

If the account isn't yours, or Repair stopped with another message, someone else may have put files in the folder. Then, as an administrator, move everything in it to a safe place and check it. Delete the folder and run Repair, which creates it again. If you aren't sure, ask for help first ([section 6.3](#63-get-help)).

**Other refusals: event IDs 5 and 6.** These stop the service with the same error 3. The temp log, `C:\Windows\Temp\lodestar-service.log`, gives the exact reason. To read it, open an [elevated PowerShell](glossary.md#elevated-powershell), as in step 1 above, and run:

```powershell
Get-Content "C:\Windows\Temp\lodestar-service.log" -Tail 20
```

**(Newer than v0.5.1)** Event ID **5** means a path setting isn't a full path. It's one of `LODESTAR_DATA_DIR`, `LODESTAR_DHT_WITNESS_DIR`, `--config` or `%ProgramData%`. In the config file, it's `data_dir`, `restart_witness_dir` or a `blocklist` file. A full path starts with a drive letter, a colon and a backslash, such as `D:\Lodestar`. The temp log names the setting. Set it to a full path or remove it, then run `Start-Service Lodestar`.

In `config.toml`, write the path as `'D:\Lodestar'` (single quotes) or `"D:/Lodestar"`. `"D:\Lodestar"` in double quotes is an error.

**(Newer than v0.5.1)** Event ID **6** means the service can't use its data folder. The note names the folder and the setting that chose it: `LODESTAR_DATA_DIR`, `data_dir` in the config file, or the default, `C:\ProgramData\Lodestar`. It also says what it found, in one of these phrases:

| The note says the folder | What to do |
|---|---|
| is a file, not a folder | Move or rename that file. For the default folder, the service then creates the folder at its next start. |
| can't be read by the service account | For the default folder, run the installer's **Repair**. For your own folder, see below. |
| can't be written to by the service account | For the default folder, run **Repair**. If Repair stops and `icacls "C:\ProgramData\Lodestar"` shows a `(DENY)` entry, remove it with `icacls "C:\ProgramData\Lodestar" /remove:d "<account>"`, using the account it shows, then run **Repair** again. For your own folder, see below. Windows Security's **Controlled folder access** can also block writes: allow `lodestar.exe` there, or use a folder it doesn't protect. |
| can't take new files | The drive may be full or write-protected. The temp log names Windows' error. Free some space or make the drive writable, or set the setting to a folder on another drive. |
| can't be created | A file is in the way, or the drive doesn't exist. The temp log names the file in the way; move it away. For the default folder, don't create the folder yourself: make sure `C:\ProgramData` is a folder, then run **Repair**. For your own folder, create it, then see below. |

**Recommended:** use the default folder. The service keeps it private. To go back to it, remove `LODESTAR_DATA_DIR` or `data_dir`, then run `Start-Service Lodestar`. Your library stays in the old folder, and the default folder starts empty. To bring the library with you, back it up first and restore it into the default folder ([section 3.7](03-keep-lodestar-safe.md#37-back-up-and-restore)).

If you keep your own folder, such as `D:\Lodestar`, make it private to the **service account**, `NT SERVICE\Lodestar`, the account Windows runs Lodestar's service as. A new folder shares the permissions of the drive it's on, which usually let every user read it.

> [!WARNING]
> The command below stops the folder, and everything in it, from inheriting other accounts' access from the drive. Use it only on a folder that holds nothing but Lodestar's data. Never give **Everyone** or **Users** access to the folder instead: it holds your library, your password and your keys. Give access to `NT SERVICE\Lodestar` only, as below.

In an [elevated PowerShell](glossary.md#elevated-powershell), run:

```powershell
icacls "D:\Lodestar" /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "NT SERVICE\Lodestar:(OI)(CI)F"
```

**Result:** `icacls "D:\Lodestar"` lists only `NT AUTHORITY\SYSTEM`, `BUILTIN\Administrators` and `NT SERVICE\Lodestar`. If it lists another account, remove that entry: `icacls "D:\Lodestar" /remove:g "<account>"` for an allow entry, or `/remove:d "<account>"` for a `(DENY)` entry. For a `(DENY)` entry for **Everyone**, use `/remove:d "*S-1-1-0"`, which works in any language of Windows. Then run `Start-Service Lodestar`. To undo the change, run `icacls "D:\Lodestar" /inheritance:e`.

If File Explorer later says you don't have permission to open the folder, select **Cancel**, not **Continue**: **Continue** adds your own account to the folder.

## 6.3 Get help

Open an issue at <https://github.com/dvortsis/lodestar-v2/issues>. Include your Lodestar version, install method, what you expected and what happened. Add the last lines of the log with the setup token, IP addresses and torrent names removed.

<!-- nav:start -->
---

[← Chapter 5: Use Lodestar](05-use-lodestar.md) · [Contents](README.md#contents) · [Chapter 7: Connect other apps →](07-connect-other-apps.md)
<!-- nav:end -->
