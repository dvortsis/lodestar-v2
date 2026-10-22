# DOC-install: defaults chosen

- **Guide location:** `docs/user/`, as the `user-docs` skill says. `docs/setup.facts.md` holds the discover output the facts came from.
- **PUID/PGID:** the Unraid templates show them, but the image doesn't read them (`packaging/docker/README.md`). The guide tells Unraid readers to add `--user 99:100` to Extra Parameters and says the fields are not read yet. If the image gains PUID/PGID support, update chapter 2 and Appendix B.
- **Unraid install:** Community Applications doesn't list Lodestar, so the guide has the reader download the template into `/boot/config/plugins/dockerMan/templates-user/` (`docs/operations.md` §2).
- **Recommended DHT choice:** no router forwarding. Forwarding UDP 6881 is optional and the guide names the trade-off (IP visible to DHT peers).
- **Stub chapters:** 4 (Windows and Linux) and 5 (Use Lodestar) hold one line each, marked (planned). Chapter 6 (Troubleshooting) is written because the lint gate requires it.
- **Planned features** (in-app log controls, Download diagnostics, update settings, `lodestar backup`) are marked (planned); backups are manual copies with Lodestar stopped.
- **Lint warnings left:** HEADING_CASE on "Install Lodestar on Unraid and Docker" (product names).
