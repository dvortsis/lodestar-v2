# Chapter 4: Install on Windows and Linux

Install v0.5.1 from the published Windows or Linux downloads. Check the download first, then choose a service or a portable run. The last two sections explain two warnings Lodestar can show about your PC.

> [!NOTE]
> **What you will learn**
> - Explain why Windows shows "Windows protected your PC" for Lodestar.
> - Check a download against its [checksum](glossary.md#checksum) with PowerShell.
> - Decide when to run the file and when to stop.
> - Install the Windows service, portable ZIP or Linux service, and say what each leaves on your PC.
> - Move a data folder off a slow or unsafe filesystem.
> - Fix the Windows port warning with the `netsh` command Insights gives you.

## 4.1 Understand the SmartScreen warning
<!-- index: SmartScreen; unsigned; code signing -->

Windows builds of Lodestar ship **unsigned** for now, because the project has no code-signing certificate. [SmartScreen](glossary.md#smartscreen) shows "Windows protected your PC" for apps it doesn't recognise, and for apps without a publisher signature. The warning doesn't mean the file is harmful. It means Windows can't tell who made it.

That is why you verify the file yourself before you run it.

## 4.2 Check the download before you run it
<!-- index: checksum; Get-FileHash; SHA-256 -->

A [checksum](glossary.md#checksum) is a short fingerprint of a file. If the file changes by one byte, the fingerprint changes. Download Lodestar only from the project's official release page: [v0.5.1 downloads](https://github.com/dvortsis/lodestar-v2/releases/tag/v0.5.1).

> [!WARNING]
> Don't run an installer from a mirror, forum post or re-upload. Get it from the official release page instead.

Each release has one file called `SHA256SUMS` beside the installer and archives. Its lines look like `<hash>  <file name>`. Use releases only. Nightly builds are workflow artifacts with a `.sha256` file each and no signature.

To check the download on Windows:

1. On the release page, download the installer and `SHA256SUMS`. Put both in your **Downloads** folder.
2. Open **PowerShell** from the Start menu.
3. Paste this command. Replace `<file>` with the installer's file name:

    ```powershell
    (Get-FileHash "$HOME\Downloads\<file>" -Algorithm SHA256).Hash.ToLower()
    ```

   **Result:** PowerShell prints 64 letters and digits.
4. Open `SHA256SUMS` in Notepad and find the line for your installer.
5. Compare the two values character by character.
   **Result:** They are identical.

To check a download on Linux:

1. Open a terminal in the folder that holds the file and `SHA256SUMS`.
2. Run:

    ```bash
    sha256sum --check --ignore-missing SHA256SUMS
    ```

   **Result:** The line for your file ends in `OK`.

The checksum checks file integrity. The signature checks that the checksum list was signed with the project's release key. v0.5.1 includes `SHA256SUMS.minisig`. The public key has ID `02910813433E81E0`. With [minisign](https://jedisct1.github.io/minisign/) installed, check the signature before relying on the checksums:

1. Open a terminal in a folder that holds `SHA256SUMS` and `SHA256SUMS.minisig`.
2. Download the public key. In Windows PowerShell, use `curl.exe` in place of `curl`:

    ```bash
    curl --fail --silent --show-error --location --remote-name https://raw.githubusercontent.com/dvortsis/lodestar-v2/v0.5.1/packaging/keys/lodestar-release.pub
    ```

3. Run:

    ```bash
    minisign -V -p lodestar-release.pub -m SHA256SUMS
    ```

   Add `-x <path>/SHA256SUMS.minisig` if the signature is in another folder.
   **Result:** minisign prints `Signature and comment signature verified`.

## 4.3 Run it or stop
<!-- index: Run anyway; More info -->

If the hashes match, run the file. When SmartScreen appears, select **More info**, check the file name, then select **Run anyway**.

If the hashes differ, stop. Delete the file and download it again from the official release page. If it still differs, don't run it, and report it at <https://github.com/dvortsis/lodestar-v2/issues>.

> [!TIP]
> Select **Run anyway** only after the hashes match. Never turn off SmartScreen or Defender to get past the warning.

## 4.4 Install on Windows
<!-- index: Windows; MSI; portable ZIP; Windows service; Defender; uninstall; REMOVE_DATA -->

Choose a download from the [v0.5.1 release](https://github.com/dvortsis/lodestar-v2/releases/tag/v0.5.1). Both Windows packages require `x86_64`.

| Option | When to use it |
|---|---|
| **MSI installer (recommended)** | Run Lodestar as a service that starts with Windows, with firewall rules and a Start-menu shortcut. Download `lodestar-0.5.1-x86_64.msi`. |
| Portable ZIP | Try Lodestar without installing anything. Download `lodestar-0.5.1-x86_64-windows-portable.zip`. |

### Install the MSI

The installer shows the licence, then one **Options** page, then **Ready**. It has no folder page: the program always goes in `C:\Program Files\Lodestar`, so the scripts Windows runs as SYSTEM stay out of reach of other users.

1. Verify the MSI as described in [section 4.2](#42-check-the-download-before-you-run-it), then open it.
2. Accept the licence. On **Options**, leave both boxes unchecked unless you have a reason:

    - **Exclude Lodestar's data folder from Windows Defender scans.** Defender then skips `C:\ProgramData\Lodestar`. Lodestar writes its database and index all day, so scanning every write can slow it several times over. **Recommended:** leave it off. Lodestar works without it, only slower. If you turn it on, only that folder is excluded, never a drive.
    - **Let other PCs on my private network open the Lodestar web page.** This opens `8090/tcp` in the firewall, for private networks only. The page on this PC works either way. **Recommended:** leave it off unless another trusted computer on your home network needs the page.

3. Finish the installer and accept the Windows administrator prompt for this verified installer.
   **Result:** The **Lodestar** service starts. Its files are in `C:\Program Files\Lodestar`, and its data is in `C:\ProgramData\Lodestar`.
4. From Start, open **PowerShell** with **Run as administrator**, then read the [setup token](glossary.md#setup-token):

    ```powershell
    Get-Content -LiteralPath 'C:\ProgramData\Lodestar\logs\lodestar.log' | Select-String 'setup token'
    ```

5. Open `http://localhost:8090/` and complete [first-run setup](02-install-lodestar-on-unraid-and-docker.md#25-create-the-admin-password).
   **Result:** You sign in with your new password. Keep the token and log private.

![Windows installer options showing both optional checkboxes left unchecked](images/windows-options-illustration.svg)
*Figure 4.1: Recommended Windows installer options. Illustration, not a live screenshot.*

You can also set the options from the command line, for an unattended install. The two properties are `DEFENDER_EXCLUSION=1` and `WEB_FIREWALL=1`, for example `msiexec /i lodestar-0.5.1-x86_64.msi DEFENDER_EXCLUSION=1`. You can add or remove the Defender exclusion later in **Windows Security → Virus & threat protection → Manage settings → Exclusions**.

### What the installer sets up

| Part | What it is |
|---|---|
| Service | **Lodestar**, running as its own unprivileged account, `NT SERVICE\Lodestar`. It starts a little after Windows boots (**Automatic (Delayed Start)**) and runs `lodestar.exe serve`. |
| After a crash | Windows restarts it up to twice, 60 seconds apart. It then waits a day before it will restart it again, so check a service that stays stopped ([section 6.1](06-troubleshooting.md#61-common-problems)). |
| Program | `C:\Program Files\Lodestar` |
| Data | `C:\ProgramData\Lodestar`. Only SYSTEM, Administrators and the service can read it. Logs are in its `logs` folder. |
| Firewall | `6881/udp` ([UDP](glossary.md#udp), for the [DHT](glossary.md#dht)) for the Lodestar program on private networks only. `8090/tcp` only if you ticked the web-page option. |
| Start menu | A **Lodestar** shortcut that opens `http://localhost:8090/` in your browser. |

![The Lodestar service in Windows Services: Automatic (Delayed Start), running as NT SERVICE\Lodestar](images/windows-service-illustration.svg)
*Figure 4.2: The Lodestar service as Windows Services shows it. Illustration, not a live screenshot.*

> [!WARNING]
> Do not open `C:\ProgramData\Lodestar` in File Explorer, and never select **Continue** on its permission prompt. Doing so adds your account permanently, and the service then refuses to start. Use [elevated PowerShell](glossary.md#elevated-powershell) instead. If this already happened, follow [the error 3 fix](06-troubleshooting.md#62-fix-windows-service-error-3).

An elevated PowerShell shows the service state at any time:

```powershell
Get-Service Lodestar
```

**Result:** `Status` shows `Running`.

### Uninstall the MSI

Open **Settings → Apps → Installed apps**, find **Lodestar**, and select **Uninstall**.

**Result:** The service, program, firewall rules, Start-menu shortcut and any Defender exclusion the installer added are removed. **Your data stays** in `C:\ProgramData\Lodestar`: your library, password and keys. Reinstalling picks it up again.

An uninstall from the elevated PowerShell that also deletes the data uses `REMOVE_DATA=1`:

```powershell
msiexec /x lodestar-0.5.1-x86_64.msi REMOVE_DATA=1
```

> [!WARNING]
> `REMOVE_DATA=1` deletes your library, password and keys, and you can't undo it. Take a [backup](03-keep-lodestar-safe.md#37-back-up-and-restore) first if you might want any of it. If the folder holds a link or junction, the installer leaves it and the uninstall still finishes.

### Use the portable ZIP

The portable ZIP installs nothing: no service, no firewall rules, no changes to Windows. Delete the folder to remove it.

1. Verify the ZIP, then extract it into a private folder inside your own user folder, such as **Documents**. Use a local SSD. Don't use the top of a drive, a shared drive, or a synced folder such as OneDrive.
2. Open **Start Lodestar.cmd**. A console window opens and then your browser opens `http://localhost:8090/`. If Windows asks about the firewall, allow private networks only.
3. Copy the setup token from the console and complete setup in the browser.
   **Result:** Lodestar keeps its database and settings in the `data` folder beside `lodestar.exe`.
4. To stop, press **Ctrl+C** in the console and wait for the line "Lodestar stopped." before you close the window. Windows may ask "Terminate batch job (Y/N)?"; answer **N**.
   **Result:** Lodestar saves its database and index before exiting.

How it differs from the service:

| | MSI service | Portable ZIP |
|---|---|---|
| Starts with Windows | Yes | No: run **Start Lodestar.cmd** each time |
| Runs as | Its own account, `NT SERVICE\Lodestar` | You |
| Data | `C:\ProgramData\Lodestar`, readable by Administrators only | `data` folder beside `lodestar.exe`, readable by anyone who can read that folder |
| Firewall rules | Added for you | Windows asks the first time |
| Remove | Uninstall, as above | Delete the folder |

**Recommended:** use the MSI for a PC that stays on. Use the ZIP to try Lodestar.

## 4.5 Install on Linux
<!-- index: Linux; systemd; service; tarball; journalctl -->

Download `lodestar-0.5.1-x86_64-linux-musl.tar.gz` for Intel or AMD Linux, or `lodestar-0.5.1-aarch64-linux-musl.tar.gz` for ARM64 Linux. Each holds one program, `lodestar`, with the web page built in, plus `lodestar.service`, `README.txt` and `LICENSE`. It needs no other packages.

**Recommended:** use the included systemd service for an installation that starts at boot. The unit is hardened: it runs as an unprivileged dynamic user (`DynamicUser=yes`), with no extra privileges and a read-only system.

To install the Linux service:

1. Download the archive, `SHA256SUMS` and `SHA256SUMS.minisig` into a private folder. Verify them using [section 4.2](#42-check-the-download-before-you-run-it).
2. Open a terminal in that folder and extract the archive:

    ```bash
    tar -xzf lodestar-0.5.1-<arch>-linux-musl.tar.gz
    cd lodestar-0.5.1-<arch>-linux-musl
    ```

   Replace `<arch>` in both commands with `x86_64` or `aarch64`, matching your download.
3. In the same terminal, install the program and service:

    ```bash
    sudo install -m 0755 lodestar /usr/local/bin/lodestar
    sudo install -m 0644 lodestar.service /etc/systemd/system/lodestar.service
    sudo systemctl daemon-reload
    sudo systemctl enable --now lodestar
    ```

   **Result:** The service starts at once and at every boot. It keeps its data in `/var/lib/lodestar`, which systemd creates for it, and no other user can read it.
4. Read the token in the same terminal:

    ```bash
    sudo journalctl -u lodestar --no-pager | grep 'setup token'
    ```

5. Open `http://localhost:8090/` on that machine, or `http://<server-ip>:8090/` on your trusted home network, and complete setup.
   **Result:** You sign in and can search the local index.

Replace `<server-ip>` with your server's address. If a firewall blocks the web page, allow `8090/tcp` only from your trusted home network. Never forward it on your router.

Day to day:

| To | Run |
|---|---|
| See whether it runs | `systemctl status lodestar` |
| Read the log | `sudo journalctl -u lodestar` |
| Follow the log as it grows | `sudo journalctl -u lodestar -f` |
| Restart or stop it | `sudo systemctl restart lodestar` or `sudo systemctl stop lodestar` |

The service has a second private folder, `/var/lib/lodestar-witness`, for the restart record. Don't back it up together with the data folder.

To try Linux without a service, open a terminal in the extracted folder and run `./lodestar serve` as your normal user. It stores data in `./data`. Stop with **Ctrl+C** and wait for shutdown.

| Mistake | What to do instead |
|---|---|
| Install an archive for the wrong processor | Match `x86_64` or `aarch64` to your Linux machine. |
| Run a portable copy from a shared or synced folder | Use a private folder on local SSD storage. |
| Close a running console without stopping Lodestar | Press **Ctrl+C** and wait for shutdown. |

## 4.6 Fix a data folder warning
<!-- index: filesystem warning; FUSE; user share; network filesystem; data folder -->

Lodestar's database and search index do many small reads and writes, and the database relies on file locking. Some filesystems are slow at this or can't lock files reliably. At start, Lodestar checks which filesystem holds its data folder. It warns you in three places:

- the **Storage & retention** step of the setup wizard **(newer than v0.5.1)**;
- **Settings → About**;
- **Insights → Overview**, as an exception **(newer than v0.5.1)**.

A warning never stops you. It tells you what to move and where.

| What the warning says | Why it matters | What to do |
|---|---|---|
| **Unraid's user share** (`/mnt/user`) | The share goes through Unraid's [FUSE](glossary.md#fuse) layer, which is slow for the index and risky for the database. | Move the data folder to a pool path. In the [container's](glossary.md#container) template, set the data folder's host path to one such as `/mnt/cache/appdata/lodestar` ([section 2.2](02-install-lodestar-on-unraid-and-docker.md#22-settings-in-the-unraid-template)). Keep only backups on the array. |
| **A FUSE filesystem** | FUSE filesystems such as mergerfs or ntfs-3g are slow for the index and may not give the database the locking it needs. | Keep the data folder on a local disk, ideally an SSD. |
| **A [network or shared-folder filesystem](glossary.md#network-or-shared-folder-filesystem)** | NFS, SMB, a Windows network drive, and the shared folders WSL2 and Docker Desktop mount from a Windows drive can't lock files reliably enough, and the database can be damaged. | Keep the data folder on a local disk, ideally an SSD. Send only backups to the share. |

To move a data folder you already use:

1. Stop Lodestar. On Unraid, stop the container. On Linux, run `sudo systemctl stop lodestar`.
2. Copy the whole data folder to the new place, and check the copy.
3. Point Lodestar at the new folder: on Unraid, the template's **Data** host path; on Docker, the volume or bind mount; on Linux, `LODESTAR_DATA_DIR`.
4. Start Lodestar and check that the warning is gone and your library is there. Keep the old folder until you're sure.
   **Result:** The warning no longer appears and your library is unchanged.

> [!TIP]
> The warning's detail line can name the filesystem type, such as `fuse.shfs` or `nfs4`. You don't need it to fix the problem. Include it if you ask for help.

## 4.7 Fix the Windows port warning
<!-- index: ephemeral ports; netsh; dynamic port range; port warning -->

Every connection Lodestar opens to a peer uses a temporary local port, taken from the [dynamic port range](glossary.md#dynamic-port-range) Windows sets aside. Hyper-V, WSL and Docker Desktop can reserve blocks inside that range.

When too few ports are left, fetches fail and other programs on the PC can lose their network access. **Insights → Overview** lists this as an exception **(newer than v0.5.1)**, and Lodestar logs it at start. It checks again about once an hour.

The sentence tells you how many ports are left and how many Lodestar and the rest of this PC need. It then gives one of two fixes.

**A command to run.** The exception shows a command with the numbers filled in for your PC. `ipv4` becomes `ipv6` when that range has the problem. Its form, for an administrator Command Prompt or PowerShell, is:

```powershell
netsh int ipv4 set dynamicport tcp start=<start> num=<count>
```

It moves where Windows' temporary range starts and how big it is, so enough ports are left for everything. Copy the command from the exception, not from this page: the numbers depend on your PC.

1. Open **Start**, type **Command Prompt**, right-click it and select **Run as administrator**. PowerShell as administrator works too.
2. Paste the command from the exception and press Enter.
   **Result:** `netsh` prints `Ok.`
3. Check the range:

    ```powershell
    netsh int ipv4 show dynamicport tcp
    ```

   **Result:** `Start Port` and `Number of Ports` match the command you ran. Use `ipv6` in place of `ipv4` if the exception named IPv6.

The exception clears within an hour. If it stays, run `netsh int ipv4 show excludedportrange protocol=tcp` to see which blocks other programs reserve.

> [!WARNING]
> The command changes Windows' network settings for the whole PC. Run it only from an exception on your own PC, as an administrator. To undo it, run `netsh int ipv4 set dynamicport tcp start=49152 num=16384`, Windows' default range.

**No command.** When no range is large enough, the exception says to lower the crawler's intensity in Settings, then restart Lodestar. Open **Settings → Crawler**, set **Crawl intensity** one step lower, save, and restart. The change applies at the next start.

## 4.8 Check your understanding

1. Why does the MSI installer have no folder page, and where does the program go?
2. What stays on your PC after you uninstall the MSI, and how do you remove it as well?
3. Your data folder is on Unraid's `/mnt/user`. Where should it go, and why?
4. Insights shows the ports exception with no command. What do you do?

Answers are in [Appendix A](appendix-a-answers.md#chapter-4-install-on-windows-and-linux).

> [!TIP]
> **Key takeaways**
> - Windows builds are unsigned, so SmartScreen warns.
> - Compare the `Get-FileHash` value with the checksum before you run anything.
> - If the hashes differ, don't run the file.
> - The MSI is the recommended Windows install; uninstalling keeps your data unless you ask otherwise.
> - A data folder on a share or FUSE filesystem belongs on a local disk.

<!-- nav:start -->
---

[← Chapter 3: Keep Lodestar safe](03-keep-lodestar-safe.md) · [Contents](README.md#contents) · [Chapter 5: Use Lodestar →](05-use-lodestar.md)
<!-- nav:end -->
