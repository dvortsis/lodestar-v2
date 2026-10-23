# Chapter 4: Install on Windows and Linux

Windows and Linux installs are **(planned)**: nothing is published yet. This chapter explains what to expect from the unsigned Windows download, so you can check it before you run it.

> [!NOTE]
> **What you will learn**
> - Explain why Windows shows "Windows protected your PC" for Lodestar.
> - Check a download against its [checksum](glossary.md#checksum) with PowerShell.
> - Decide when to run the file and when to stop.

## 4.1 Understand the SmartScreen warning
<!-- index: SmartScreen; unsigned; code signing -->

Windows builds of Lodestar ship **unsigned** for now, because the project has no code-signing certificate. [SmartScreen](glossary.md#smartscreen) shows "Windows protected your PC" for apps it doesn't recognise, and for apps without a publisher signature. The warning doesn't mean the file is harmful. It means Windows can't tell who made it.

That is why you verify the file yourself before you run it.

## 4.2 Check the download before you run it
<!-- index: checksum; Get-FileHash; SHA-256 -->

A [checksum](glossary.md#checksum) is a short fingerprint of a file. If the file changes by one byte, the fingerprint changes. Download Lodestar only from the project's official release page: <https://github.com/dvortsis/lodestar-v2/releases>.

> [!WARNING]
> Don't run an installer from a mirror, forum post or re-upload. Get it from the official release page instead.

To check the file:

1. On the release page, download the installer and the signed checksum file listed with it **(planned)**. The exact file name is not decided yet.
2. Open **PowerShell** from the Start menu.
3. Paste this command. Replace `<path-to-installer>` with the full path of the file you downloaded:

    ```powershell
    Get-FileHash -Algorithm SHA256 "<path-to-installer>"
    ```

   **Result:** PowerShell prints a `Hash` value of 64 letters and digits, in capitals.
4. Open the checksum file and find the line for your installer.
5. Compare the two values character by character. Capitals and lowercase letters count as the same.
   **Result:** They are identical.

The checksum file is signed so you can tell it came from the project. How to verify that signature is **(planned)** and will be added with the first Windows release.

## 4.3 Run it or stop
<!-- index: Run anyway; More info -->

If the hashes match, run the file. When SmartScreen appears, select **More info**, check the file name, then select **Run anyway**.

If the hashes differ, stop. Delete the file and download it again from the official release page. If it still differs, don't run it, and report it at <https://github.com/dvortsis/lodestar-v2/issues>.

> [!TIP]
> Select **Run anyway** only after the hashes match. Never turn off SmartScreen or Defender to get past the warning.

## 4.4 Install on Linux

For Linux, `packaging/systemd/README.md` describes the service unit. Release files are **(planned)**.

> [!TIP]
> **Key takeaways**
> - Windows builds are unsigned, so SmartScreen warns.
> - Compare the `Get-FileHash` value with the checksum before you run anything.
> - If the hashes differ, don't run the file.

<!-- nav:start -->
---

[← Chapter 3: Keep Lodestar safe](03-keep-lodestar-safe.md) · [Contents](README.md#contents) · [Chapter 5: Use Lodestar →](05-use-lodestar.md)
<!-- nav:end -->
