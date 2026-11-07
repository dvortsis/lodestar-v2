# Chapter 4: Install on Windows and Linux

Install v0.5.1 from the published Windows or Linux downloads. Check the download first, then choose a service or a portable run.

> [!NOTE]
> **What you will learn**
> - Explain why Windows shows "Windows protected your PC" for Lodestar.
> - Check a download against its [checksum](glossary.md#checksum) with PowerShell.
> - Decide when to run the file and when to stop.
> - Install the Windows service, portable ZIP or Linux service.

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
<!-- index: Windows; MSI; portable ZIP; Windows service; Defender -->

Choose a download from the [v0.5.1 release](https://github.com/dvortsis/lodestar-v2/releases/tag/v0.5.1). Both Windows packages require `x86_64`.

| Option | When to use it |
|---|---|
| **MSI installer (recommended)** | Run Lodestar as a service that starts with Windows. Download `lodestar-0.5.1-x86_64.msi`. |
| Portable ZIP | Try Lodestar without installing a service. Download `lodestar-0.5.1-x86_64-windows-portable.zip`. |

To install the Windows service:

1. Verify the MSI as described in [section 4.2](#42-check-the-download-before-you-run-it), then open it.
2. On **Options**, leave **Exclude Lodestar's data folder from Windows Defender scans** unchecked.
3. Leave **Let other PCs on my private network open the Lodestar web page** unchecked unless you need access from another trusted computer.
4. Finish the installer and accept the Windows administrator prompt for this verified installer.
   **Result:** The **Lodestar** service starts. Its files are in `C:\Program Files\Lodestar`, and its data is in `C:\ProgramData\Lodestar`.
5. From Start, open **PowerShell** with **Run as administrator**, then read the [setup token](glossary.md#setup-token):

    ```powershell
    Get-Content -LiteralPath 'C:\ProgramData\Lodestar\logs\lodestar.log' | Select-String 'setup token'
    ```

6. Open `http://localhost:8090/` and complete [first-run setup](02-install-lodestar-on-unraid-and-docker.md#25-create-the-admin-password).
   **Result:** You sign in with your new password. Keep the token and log private.

![Windows installer options showing both optional checkboxes left unchecked](images/windows-options-illustration.svg)
*Figure 4.1: Recommended Windows installer options. Illustration, not a live screenshot.*

The service uses its own unprivileged account. Only Administrators, SYSTEM and the service can read its data folder. Read logs from [elevated PowerShell](glossary.md#elevated-powershell).

> [!WARNING]
> Do not open `C:\ProgramData\Lodestar` in File Explorer, and never select **Continue** on its permission prompt. Doing so adds your account permanently, and the service then refuses to start. Use elevated PowerShell instead. If this already happened, follow [the error 3 fix](06-troubleshooting.md#62-fix-windows-service-error-3).

The installer opens [DHT](glossary.md#dht) port `6881` over [UDP](glossary.md#udp) on private networks only. It opens `8090/tcp` to other computers only when you select the web-page option. Uninstalling keeps your data by default.

**Recommended:** leave the Defender exclusion off. If you later need it for performance, exclude only `C:\ProgramData\Lodestar`, never a whole drive.

To use the portable ZIP:

1. Verify the ZIP, then extract it into a private folder on a local SSD, outside OneDrive or other synced folders.
2. Open **Start Lodestar.cmd**. If Windows asks about the firewall, allow private networks only.
3. Copy the setup token from its console and complete setup at `http://localhost:8090/`.
   **Result:** Lodestar keeps its database and settings in the `data` folder beside `lodestar.exe`.
4. To stop, press **Ctrl+C** in the console and wait for shutdown before closing it.
   **Result:** Lodestar saves its database and index before exiting.

## 4.5 Install on Linux
<!-- index: Linux; systemd; service; tarball -->

Download `lodestar-0.5.1-x86_64-linux-musl.tar.gz` for Intel or AMD Linux, or `lodestar-0.5.1-aarch64-linux-musl.tar.gz` for ARM64 Linux.

**Recommended:** use the included systemd service for an installation that starts at boot. It runs as an unprivileged dynamic user.

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

   **Result:** The service starts and keeps its data in `/var/lib/lodestar`.
4. Read the token in the same terminal:

    ```bash
    sudo journalctl -u lodestar --no-pager | grep 'setup token'
    ```

5. Open `http://localhost:8090/` on that machine, or `http://<server-ip>:8090/` on your trusted home network, and complete setup.
   **Result:** You sign in and can search the local index.

Replace `<server-ip>` with your server's address. If a firewall blocks the web page, allow `8090/tcp` only from your trusted home network. Never forward it on your router.

To try Linux without a service, open a terminal in the extracted folder and run `./lodestar serve` as your normal user. It stores data in `./data`. Stop with **Ctrl+C** and wait for shutdown.

| Mistake | What to do instead |
|---|---|
| Install an archive for the wrong processor | Match `x86_64` or `aarch64` to your Linux machine. |
| Run a portable copy from a shared or synced folder | Use a private folder on local SSD storage. |
| Close a running console without stopping Lodestar | Press **Ctrl+C** and wait for shutdown. |

> [!TIP]
> **Key takeaways**
> - Windows builds are unsigned, so SmartScreen warns.
> - Compare the `Get-FileHash` value with the checksum before you run anything.
> - If the hashes differ, don't run the file.

<!-- nav:start -->
---

[← Chapter 3: Keep Lodestar safe](03-keep-lodestar-safe.md) · [Contents](README.md#contents) · [Chapter 5: Use Lodestar →](05-use-lodestar.md)
<!-- nav:end -->
