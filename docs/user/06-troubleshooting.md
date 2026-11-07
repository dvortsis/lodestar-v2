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
| The Windows service won't start and Windows shows "service-specific error 3" | The data folder `C:\ProgramData\Lodestar` isn't private any more, most often because **Continue** was selected on its permission prompt in File Explorer | Follow [section 6.2](#62-fix-windows-service-error-3). |
| Windows logs three failure events for one problem | Windows restarts the service twice after it stops with an error, 60 seconds apart, so one problem shows three times | Fix the cause once, as in [section 6.2](#62-fix-windows-service-error-3). The count starts again after a day. |
| Lodestar logs the warning `logging.folder_shared` on Windows | The `logs` folder has an extra account in its permissions | Remove that account as in step 3 of [section 6.2](#62-fix-windows-service-error-3), then run `Restart-Service Lodestar` in the same [elevated PowerShell](glossary.md#elevated-powershell) window. |

## 6.2 Fix Windows service error 3
<!-- index: error 3; Windows service; icacls; folder_shared; service-specific error 3 -->

At every start the Lodestar service checks that `C:\ProgramData\Lodestar` is private: only SYSTEM, Administrators and `NT SERVICE\Lodestar` have permissions, nothing is inherited, and nothing inside is a link. If the check fails, the service refuses to start. Windows shows only "service-specific error 3". The reason goes to `C:\Windows\Temp\lodestar-service.log`, which only administrators can read.

> [!WARNING]
> Don't delete the folder, even if the log suggests it: it holds your library, your password and your keys. Don't run the installer's Repair either: it also refuses a folder that isn't private and already holds data. The steps below remove only the extra account.

To fix it:

1. Open an [elevated PowerShell](glossary.md#elevated-powershell) ([section 4.4](04-install-on-windows-and-linux.md#44-install-on-windows)). Read the reason:

    ```powershell
    Get-Content "C:\Windows\Temp\lodestar-service.log" -Tail 20
    ```

   **Result:** The last line says the folder isn't private and why, for example that it grants access to an account shown as a long `S-1-5-21-…` number. It also suggests Repair or deleting the folder. Do neither: Repair refuses this folder, and deleting it loses your library and keys.
2. See which account was added. Replace `<user name>` with your Windows user name:

    ```powershell
    icacls "C:\ProgramData\Lodestar" /T | Select-String "<user name>"
    ```

   **Result:** Lines show your account with its rights, such as `C:\ProgramData\Lodestar DESKTOP-1\alex:(OI)(CI)(F)`. The account name is the text after the path and before `:(`.
3. Remove only that account. Replace `<account>` with the account name exactly as step 2 printed it, for example `DESKTOP-1\alex` or `AzureAD\AlexSmith`:

    ```powershell
    icacls "C:\ProgramData\Lodestar" /remove "<account>" /T /C
    ```

   If step 2 printed nothing, use the `S-1-5-21-…` number from the log in step 1 instead, with a `*` in front: `/remove "*S-1-5-21-…"`.

   **Result:** `icacls` reports the files it processed. It removes that one account's entries and changes nothing else.
4. Start the service:

    ```powershell
    Start-Service Lodestar
    ```

   Wait about 10 seconds, then check that it is still running:

    ```powershell
    Get-Service Lodestar
    ```

   **Result:** `Status` shows `Running`, and the web page loads. If it shows `Stopped`, the check refused again: repeat step 1 and read the new reason.

The check also needs the folder's owner, and the owner of each item directly inside it, to be SYSTEM, Administrators or the service. It also refuses any link or junction inside the folder. If the log names one of those, don't change it yourself: ask for help ([section 6.3](#63-get-help)).

## 6.3 Get help

Open an issue at <https://github.com/dvortsis/lodestar-v2/issues>. Include your Lodestar version, install method, what you expected and what happened. Add the last lines of the log with the setup token, IP addresses and torrent names removed.

<!-- nav:start -->
---

[← Chapter 5: Use Lodestar](05-use-lodestar.md) · [Contents](README.md#contents) · [Appendix A: Answers to check your understanding →](appendix-a-answers.md)
<!-- nav:end -->
