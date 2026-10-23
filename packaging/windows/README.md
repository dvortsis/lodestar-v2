# Windows packages

Two downloads per release, both `x86_64`:

| File | Pick it if |
|---|---|
| `lodestar-<version>-x86_64.msi` | You want Lodestar installed like any other app: Start-menu shortcut, firewall rules, a Windows service. **Recommended.** |
| `lodestar-<version>-x86_64-windows-portable.zip` | You want to try it, or run it from a folder, without changing Windows. Delete the folder to remove it. |

Check the download against `SHA256SUMS` on the release page first
(`Get-FileHash <file> -Algorithm SHA256`).

> **Not code-signed yet.** Windows SmartScreen may say "Windows protected your
> PC". If the checksum matches, choose **More info → Run anyway**. Signing
> starts when the owner adds a certificate (see [Signing](#signing)).

## What the installer does

- Installs `lodestar.exe` to `C:\Program Files\Lodestar`.
- Keeps your data in `C:\ProgramData\Lodestar`. **Uninstalling keeps it** unless
  you ask otherwise (below).
- Registers a Windows service named **Lodestar**, running as its own
  unprivileged account, `NT SERVICE\Lodestar`, set to start by hand. *Until the service entrypoint
  ships (slice M6-win), starting it from Services will fail. To run Lodestar now,
  use the portable zip, or run `lodestar.exe serve` from a terminal.*
- Locks `C:\ProgramData\Lodestar` so only SYSTEM, Administrators and the
  Lodestar service account can use it. Other users on the PC can't read your data.
- Opens two firewall ports for `lodestar.exe`, on **private networks only**:
  `8090/tcp` (the web page) and `6881/udp` (the DHT).
- Adds a Start-menu shortcut that opens `http://localhost:8090/`.
- Offers one optional box on the Options page: **exclude the data folder from
  Windows Defender scans**. It is **off** by default.

### The Defender exclusion: should you tick it?

Lodestar writes its database and search index all day, and Defender scanning
every write can slow that down several times over. Ticking the box tells
Defender to skip `C:\ProgramData\Lodestar`. The trade-off is that nothing in that
folder is scanned. Lodestar keeps only its own database and index there and
downloads torrent metadata, not files. If you share the PC or aren't sure, leave
it off: Lodestar still works, only slower. Remove the exclusion any time in
**Windows Security → Virus & threat protection → Manage settings → Exclusions**.

## Silent install and uninstall

```powershell
msiexec /i lodestar-0.5.0-x86_64.msi /qn DEFENDER_EXCLUSION=1   # box ticked; leave it out for off
msiexec /x lodestar-0.5.0-x86_64.msi /qn                        # uninstall, keep data
msiexec /x lodestar-0.5.0-x86_64.msi /qn REMOVE_DATA=1          # uninstall and delete the data
```

## Build it yourself

Needs Windows, the MSVC Rust target, [WiX Toolset 3](https://wixtoolset.org/) and
`cargo install cargo-wix --locked`. The web UI must be built first (`npm run build`
in `web/`), because it is embedded in the binary.

```powershell
cargo build --release --locked -p lodestar --no-default-features --features web --target x86_64-pc-windows-msvc
packaging\windows\build-portable.ps1 -Version 0.5.0 -OutDir dist
packaging\windows\build-msi.ps1      -Version 0.5.0 -OutDir dist
packaging\windows\smoke.ps1 -Msi dist\lodestar-0.5.0-x86_64.msi -Zip dist\lodestar-0.5.0-x86_64-windows-portable.zip
```

`smoke.ps1` changes the machine it runs on (installs and removes the product), so
use a throwaway VM or CI. It installs silently, checks the files, service, firewall
rules and shortcut, runs the installed binary and checks `/healthz`, uninstalls and
checks that the data stayed, then reinstalls with the Defender box ticked and
uninstalls with `REMOVE_DATA=1`.

## Layout

- `main.wxs`: the installer (WiX 3). `cargo-wix` supplies the version and binary folder; `build-msi.ps1` adds the licence text and calls it with `--include main.wxs`, so `Cargo.toml` needs no `[package.metadata.wix]`.
- `build-msi.ps1`, `build-portable.ps1`: the two packages.
- `portable/`: files that go into the zip (`README.txt`, `Start Lodestar.cmd`).
- `smoke.ps1`: install/run/uninstall checks. `sign.ps1`: code signing.

## Signing

`sign.ps1` signs `lodestar.exe` and the MSI with `signtool`, but only when the
repository secrets `WINDOWS_CERT_PFX_BASE64` (the `.pfx`, base64) and
`WINDOWS_CERT_PASSWORD` exist. Without them the step prints a notice and skips,
so unsigned builds keep working. **Owner action:** buy a code-signing certificate
and add the two secrets. Nothing else needs to change. The update engine (§12.2)
also checks Authenticode signatures once they exist.
