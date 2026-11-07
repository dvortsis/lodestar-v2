# Chapter 6: Troubleshooting

This chapter lists common problems with fixes, then says how to get help.

## 6.1 Common problems

| Problem | Likely cause | Fix |
|---|---|---|
| The web page doesn't load | The [container](glossary.md#container) isn't running, or the port is taken | Check that the container shows as started. On Unraid, open the Docker tab. With Compose, run `docker compose ps` on the Docker host. If another app uses `8090`, change the host side of the **Web UI** port. |
| Lodestar exits with "started as root without the SETUID/SETGID capabilities" | The container was started with those permissions removed | In Compose, remove `cap_drop: ALL`, or add `cap_add: [SETUID, SETGID, CHOWN]` after it ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| Lodestar exits saying the data folder "doesn't look like Lodestar's" | **Data** holds other files and belongs to someone else | Point **Data** at an empty folder, such as `/mnt/cache/appdata/lodestar` ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| Lodestar exits saying "a parent folder doesn't let it in" | The user in **PUID** can't open a folder above **Data** | Give the parent folders execute permission for others, for example `chmod o+x /mnt/cache/appdata` in the Unraid terminal, or move **Data** to a folder that user can reach. |
| **Apply** or `docker compose up` fails with "manifest unknown" | The image name or tag is wrong | Set the repository or Compose image to `ghcr.io/dvortsis/lodestar-v2:0.5.1`, then pull it again. |
| You lost the [setup token](glossary.md#setup-token) | The log scrolled, or you haven't created a password yet | Restart the container. Read the log again and use the newest token. |
| Few new torrents appear after a restart | Sampling waits six hours after each start | Leave Lodestar running. Search remains available. The safe-restart switch on development builds is not in v0.5.1. |
| Nothing is discovered | The [DHT](glossary.md#dht) can't bootstrap, or outbound [UDP](glossary.md#udp) is blocked | Read the log for DHT errors. Check that DNS and outbound UDP work from the server. Without a forwarded port the crawler is slow but still works. |
| The database is slow or reports "database is locked" | The data folder is under `/mnt/user` or on a network share | Move the data folder to a pool path such as `/mnt/cache/appdata/lodestar` ([section 2.2](02-install-lodestar-on-unraid-and-docker.md#22-settings-in-the-unraid-template)). |
| Your home network gets slow | The crawler fills the router's connection table | Set **Intensity** to `low` and raise it step by step. |
| The gluetun variant stops after a [VPN](glossary.md#vpn) restart | A container that shares a network stays attached to the old one | Restart Lodestar after gluetun restarts. |
| The Windows service won't start and Windows shows "service-specific error 536870915" (0.5.2 and earlier: "error 3") | The data folder `C:\ProgramData\Lodestar` isn't private any more, most often because **Continue** was selected on its permission prompt in File Explorer | Follow [section 6.2](#62-fix-windows-service-error-3). |
| Windows logs three failure events for one problem | Windows restarts the service twice after it stops with an error, 60 seconds apart, so one problem shows three times | Fix the cause once, as in [section 6.2](#62-fix-windows-service-error-3). The count starts again after a day. |
| Other computers can't open the web page, or few torrents are found, on Windows | Windows treats your network as a **Public network**. Lodestar's firewall rules allow private networks only, on purpose | If this is your own home network and you trust every device on it, set it to private: **Settings → Network & internet**, select your connection, then under **Network profile type** select **Private network**. On a network you don't control, such as a café or hotel, leave it **Public**. Never add the Public profile to Lodestar's rules: that opens Lodestar to everyone on such networks. |
| Lodestar logs the warning `logging.folder_shared` on Windows | The `logs` folder has an extra account in its permissions | Remove that account as in step 3 of [section 6.2](#62-fix-windows-service-error-3), then run `Restart-Service Lodestar` in the same [elevated PowerShell](glossary.md#elevated-powershell) window. |

## 6.2 Fix Windows service error 3
<!-- index: error 3; error 536870915; 0x20000003; Windows service; icacls; Repair; Event Viewer; folder_shared; service-specific error 3 -->

At every start the Lodestar service checks that `C:\ProgramData\Lodestar` is private: only SYSTEM, Administrators and `NT SERVICE\Lodestar` have permissions, nothing is inherited, and nothing inside is a link. If the check fails, the service refuses to start and Windows shows "service-specific error 536870915" (`0x20000003`). Lodestar 0.5.2 and earlier show "error 3" instead.

The reason goes to two places:

- the **Application** log in Event Viewer, as an error from the source **Lodestar**;
- `C:\Windows\Temp\lodestar-service.log`, which only administrators can read.

The reason names the folder and the extra account. When the one problem is a single extra user account, it also gives the exact command that removes it.

> [!WARNING]
> Don't delete the folder: it holds your library, your password and your keys. The steps below remove only the extra account.

**Recommended:** if the extra account is your own administrator account, run the installer's **Repair** (step 2). It removes your account's entries and makes the folder private again. It refuses anything else, so it can't hide another problem.

To fix it:

1. Read the reason. Open **Event Viewer**, then **Windows Logs → Application**, and select the newest error with the source **Lodestar**. Or, in an [elevated PowerShell](glossary.md#elevated-powershell) ([section 4.4](04-install-on-windows-and-linux.md#44-install-on-windows)):

    ```powershell
    Get-Content "C:\Windows\Temp\lodestar-service.log" -Tail 20
    ```

   **Result:** The message says the folder isn't private and why, for example that it grants access to `DESKTOP-1\alex (S-1-5-21-…)`.
2. If the account it names is your own and it gives an `icacls` command, run **Repair**. Open **Control Panel → Programs and Features**, right-click **Lodestar**, then select **Repair**. Allow it when Windows asks for administrator permission.

   **Result:** Repair finishes without an error and starts the service. Continue with step 4. If Repair stops with an error, the folder has another problem: continue with step 3.
3. If you prefer the command, copy the `icacls` command from the message into the elevated PowerShell window and run it. It looks like this, with the account's number from the message:

    ```powershell
    icacls "C:\ProgramData\Lodestar" /remove:g *S-1-5-21-… /T /C
    ```

   **Result:** `icacls` reports the files it processed. It removes that one account's entries and changes nothing else.
4. Start the service, wait about 10 seconds, then check that it is still running:

    ```powershell
    Start-Service Lodestar
    Get-Service Lodestar
    ```

   **Result:** `Status` shows `Running`, and the web page loads. If it shows `Stopped`, the check refused again: repeat step 1 and read the new reason.

If the account isn't yours, or the message gives no `icacls` command, someone else may have put files in the folder. The message then says to move everything in it to a safe place, check it, delete the folder and run Repair, which creates it again. If you aren't sure, ask for help first ([section 6.3](#63-get-help)).

## 6.3 Get help

Open an issue at <https://github.com/dvortsis/lodestar-v2/issues>. Include your Lodestar version, install method, what you expected and what happened. Add the last lines of the log with the setup token, IP addresses and torrent names removed.

<!-- nav:start -->
---

[← Chapter 5: Use Lodestar](05-use-lodestar.md) · [Contents](README.md#contents) · [Appendix A: Answers to check your understanding →](appendix-a-answers.md)
<!-- nav:end -->
