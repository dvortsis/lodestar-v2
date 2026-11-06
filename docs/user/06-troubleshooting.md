# Chapter 6: Troubleshooting

This chapter lists common problems with fixes, explains a Magnetico import that is refused, then says how to get help.

## 6.1 Common problems

| Problem | Likely cause | Fix |
|---|---|---|
| The web page doesn't load | The [container](glossary.md#container) isn't running, or the port is taken | Check that the container shows as started. On Unraid, open the Docker tab. With Compose, run `docker compose ps` on the Docker host. If another app uses `8090`, change the host side of the **Web UI** port. |
| Lodestar exits with "started as root without the SETUID/SETGID capabilities" | The container was started with those permissions removed | In Compose, remove `cap_drop: ALL`, or add `cap_add: [SETUID, SETGID, CHOWN]` after it ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| Lodestar exits saying the data folder "doesn't look like Lodestar's" | **Data** holds other files and belongs to someone else | Point **Data** at an empty folder, such as `/mnt/cache/appdata/lodestar` ([section 2.3](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)). |
| Lodestar exits saying "a parent folder doesn't let it in" | The user in **PUID** can't open a folder above **Data** | Give the parent folders execute permission for others, for example `chmod o+x /mnt/cache/appdata` in the Unraid terminal, or move **Data** to a folder that user can reach. |
| **Apply** or `docker compose up` fails with "manifest unknown" | No release is published yet, so `latest` doesn't exist | Wait for the first release, or use the `nightly` tag to try Lodestar. It is untested. |
| You lost the [setup token](glossary.md#setup-token) | The log scrolled, or you haven't created a password yet | Restart the container. Read the log again and use the newest token. |
| Nothing is discovered | The [DHT](glossary.md#dht) can't bootstrap, or outbound [UDP](glossary.md#udp) is blocked | Read the log for DHT errors. Check that DNS and outbound UDP work from the server. Without a forwarded port the crawler is slow but still works. |
| The database is slow or reports "database is locked" | The data folder is under `/mnt/user` or on a network share | Move the data folder to a pool path such as `/mnt/cache/appdata/lodestar` ([section 2.2](02-install-lodestar-on-unraid-and-docker.md#22-settings-in-the-unraid-template)). |
| Your home network gets slow | The crawler fills the router's connection table | Set **Intensity** to `low` and raise it step by step. |
| The gluetun variant stops after a [VPN](glossary.md#vpn) restart | A container that shares a network stays attached to the old one | Restart Lodestar after gluetun restarts. |
| The Windows service won't start and Windows shows "service-specific error 3" | The data folder `C:\ProgramData\Lodestar` isn't private any more, most often because **Continue** was selected on its permission prompt in File Explorer | Follow [section 6.3](#63-fix-windows-service-error-3). |
| Windows logs three failure events for one problem | Windows restarts the service twice after it stops with an error, 60 seconds apart, so one problem shows three times | Fix the cause once, as in [section 6.3](#63-fix-windows-service-error-3). The count starts again after a day. |
| Lodestar logs the warning `logging.folder_shared` on Windows | The `logs` folder has an extra account in its permissions | Remove that account as in step 3 of [section 6.3](#63-fix-windows-service-error-3), then run `Restart-Service Lodestar`. |
| A Magnetico import is refused with `source_not_quiescent` | magneticod is running, or its last changes are still in the `-wal` file | See [section 6.2](#62-fix-a-magnetico-import-refused-as-in-use). |

## 6.2 Fix a Magnetico import refused as in use
<!-- index: Magnetico; import; source_not_quiescent; -wal file; WAL checkpoint -->

`lodestar import` refuses a Magnetico database with `source_not_quiescent` while magneticod may still be writing to it. The sign is a `database.sqlite3-wal` file that isn't empty, or a `database.sqlite3-journal` file, beside the database. The refusal changes nothing, and it stays even after magneticod stops if those files still hold changes.

The `-wal` file holds changes [SQLite](glossary.md#sqlite) hasn't written into the database file yet. magneticod writes them in, and removes or empties the file, when it stops cleanly. When that fails, for example because the disk is full, the `-wal` file keeps torrents that aren't in the database file.

To fix it:

1. If magneticod is running, stop it. Run the import again, as [the import guide](../operations.md#importing-from-magnetico-or-bitmagnet) shows.
   **Result:** The import starts. If it's still refused, go on to step 2.
2. Free some disk space on the drive that holds the database. Writing the changes in needs room.
3. Start magneticod, wait until it's running, then stop it.
   **Result:** magneticod writes the changes into the database, and the `-wal` file is gone or empty. Run the import again.

If you can't run magneticod again, write the changes in on a copy with the `sqlite3` tool instead. This is for a leftover `-wal` file only. A `-journal` file means a change was cut off part-way; steps 1 to 3 undo it when magneticod opens the database again.

1. Copy `database.sqlite3` and `database.sqlite3-wal` together to a folder with free space. The database file alone is missing the newest torrents and can be damaged.
2. Open a terminal in that folder (on Unraid, the terminal button at the top of the web page, then `cd` to the folder) and paste:
   ```bash
   sqlite3 database.sqlite3 'PRAGMA wal_checkpoint(TRUNCATE);'
   ```
   **Result:** It prints `0|0|0`, and the `-wal` file is gone or empty. A first number of `1` means another program still has the copy open. If the terminal says `sqlite3: command not found`, copy the folder to a computer that has the `sqlite3` tool from [sqlite.org](https://sqlite.org/download.html), and run the command there.
3. Import the copy: point `--path` at it, and in Docker mount its folder instead ([the import guide](../operations.md#importing-from-magnetico-or-bitmagnet)).

> [!WARNING]
> Never delete the `-wal` file: that loses the torrents in it. Write its changes in instead, with magneticod or `sqlite3` as above. A copy with its leftover `-wal` file beside it is refused like the original. Run step 2 on the copy before you import it.

## 6.3 Fix Windows service error 3
<!-- index: error 3; Windows service; icacls; folder_shared; service-specific error 3 -->

These steps apply once the Windows installer is published. At every start the Lodestar service checks that `C:\ProgramData\Lodestar` is private: only SYSTEM, Administrators and `NT SERVICE\Lodestar` have permissions, nothing is inherited, and nothing inside is a link. If the check fails, the service refuses to start. Windows shows only "service-specific error 3". The reason goes to `C:\Windows\Temp\lodestar-service.log`, which only administrators can read.

The installer's Repair doesn't fix this case. It also refuses a folder that isn't private and already holds data.

To fix it:

1. Open an [elevated PowerShell](glossary.md#elevated-powershell) ([section 4.4](04-install-on-windows-and-linux.md#44-find-the-setup-token-on-windows)). Read the reason:

    ```powershell
    Get-Content "C:\Windows\Temp\lodestar-service.log" -Tail 20
    ```

   **Result:** The last line says the folder isn't private and why.
2. See which account was added. Replace `<user name>` with your Windows user name:

    ```powershell
    icacls "C:\ProgramData\Lodestar" /T | Select-String "<user name>"
    ```

   **Result:** Lines show your account with its rights.
3. Remove only that account. Replace `<COMPUTER>` with your computer's name and `<user name>` with your user name:

    ```powershell
    icacls "C:\ProgramData\Lodestar" /remove "<COMPUTER>\<user name>" /T /C
    ```

   **Result:** `icacls` reports the files it processed. It removes that one account's entries and changes nothing else.
4. Start the service:

    ```powershell
    Start-Service Lodestar
    ```

   **Result:** The service runs, and the web page loads.

If the log names something else, such as a link inside the folder, the same check applies to it. Don't delete the folder: it holds your library.

## 6.4 Get help

Open an issue at <https://github.com/dvortsis/lodestar-v2/issues>. Include your Lodestar version, install method, what you expected and what happened. Add the last lines of the log with the setup token, IP addresses and torrent names removed.

<!-- nav:start -->
---

[← Chapter 5: Use Lodestar](05-use-lodestar.md) · [Contents](README.md#contents) · [Appendix A: Answers to check your understanding →](appendix-a-answers.md)
<!-- nav:end -->
