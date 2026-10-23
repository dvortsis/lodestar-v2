# DOC-install decisions (#91)

- **Guide location:** `docs/user/`, as the `user-docs` skill says. `docs/setup.facts.md` holds the discover output the facts came from.
- **PUID/PGID:** follows D-puid (#84). The image starts as root, applies PUID/PGID, takes ownership of `/data` and drops root. The guide says: leave 99/100 on Unraid, never 0, and no manual `chown`. The two D-puid log messages are in Troubleshooting.
- **Unraid install:** Community Applications doesn't list Lodestar, so the guide has the reader download the template into `/boot/config/plugins/dockerMan/templates-user/` (`docs/operations.md` §2).
- **Recommended DHT choice:** no router forwarding. Forwarding UDP 6881 is optional and the guide names the trade-off (IP visible to DHT peers).
- **Stub chapters:** 4 (Windows and Linux) and 5 (Use Lodestar) hold one line each, marked (planned). Chapter 6 (Troubleshooting) is written because the lint gate requires it.
- **Planned features** (in-app log controls, Download diagnostics, update settings, `lodestar backup`) are marked (planned); `lodestar backup` is built (M6-cli, #95) and documented with its restore steps; update settings, log controls and Download diagnostics stay (planned).
- **Lint warnings left:** HEADING_CASE on "Install Lodestar on Unraid and Docker" (product names).
- **Windows unsigned (runner message, owner decision 2026-10-23):** chapter 4 explains SmartScreen and a `Get-FileHash` check. The checksum file's name and location, and how to verify its signature, are not defined in the repo, so they are marked (planned). `docs/architecture.md` line 1477 still says Windows artifacts are Authenticode-signed; the runner should update it.
- **Image availability:** no `v*` tags exist, so `latest` isn't published. The guide says the image comes with the first release and that `nightly` is the only tag today, untested.
