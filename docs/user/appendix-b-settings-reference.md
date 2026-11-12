# Appendix B: Settings reference

Every field in the Lodestar Unraid template in one table. The recommended value is the safe choice for most setups.

| Setting | What it does | Recommended | Notes |
|---|---|---|---|
| Web UI | Port for the web page and API (`8090/tcp`) | `8090` | Never forward it on your router. |
| DHT / peers (UDP) | Port for DHT traffic (`6881/udp`) | `6881` | Forwarding is optional and shows your IP to DHT peers. |
| Peers (TCP) | Reserved for incoming peers (planned) | `6881` | Nothing listens yet. Don't forward it. |
| Data | Index, database and settings (`/data`) | `/mnt/cache/appdata/lodestar` | Use a pool path, not `/mnt/user`. |
| PUID | User ID Lodestar runs as; it makes Data this user's on start | `99` | Never `0` (root). |
| PGID | Group ID Lodestar runs as | `100` | Never `0` (root group). |
| TZ | Time zone | `Etc/UTC` | Log timestamps are UTC today. |
| Intensity | Crawler effort (`LODESTAR_INTENSITY`) | `balanced` | `aggressive` can overload home routers. |
| DHT port | Gluetun template only (`LODESTAR_DHT_PORT`) | `6881` | Match the port your VPN provider forwards. |

## Updates settings on Windows (newer than v0.5.1)

These are in **Settings → Updates**. Each can also be set in `config.toml` or locked by a variable. v0.5.1 has none of them.

| Setting (key) | Label | Default | Recommended | Notes |
|---|---|---|---|---|
| `updates.mode` (`LODESTAR_UPDATES`) | How to update | Off | Check and notify | Install automatically behaves like Check and notify on Windows. Container image updates is for Docker and Unraid. |
| `updates.level` (`LODESTAR_UPDATES_LEVEL`) | Which releases | Fixes only | Fixes only | Used by automatic installs, which Windows doesn't run yet. |
| `updates.security_fixes` (`LODESTAR_UPDATES_SECURITY_FIXES`) | Always install security fixes | On under Install automatically, off under Check and notify | Leave as it is | Nothing installs unattended on Windows, so it changes nothing there yet. |
| `updates.channel` (`LODESTAR_UPDATES_CHANNEL`) | Channel | Stable | Stable | Nightly is for testing only. |
| `updates.wait_days` (`LODESTAR_UPDATES_WAIT_DAYS`) | Days to wait after a release | 3 | 3 | 0 to 14. **Install now** ignores it. |
| `updates.window` (`LODESTAR_UPDATES_WINDOW`) | Install window | `03:00-05:00` | Leave as it is | Local time. Automatic installs only; **Install now** ignores it. |
| `updates.allow_reindex` (`LODESTAR_UPDATES_ALLOW_REINDEX`) | Allow search index rebuilds | On | On | Applies to automatic installs. |

## Release scope

This reference covers v0.5.1. The Sources screen, pack-version controls and safe-restart switch described in development notes are not available in this release.

The templates do not include a Log level field. You can optionally add a custom
container variable with key `LODESTAR_LOG` to set how much Lodestar logs on every output
(`error`, `warn`, `info` or `debug`). In v0.5.1, change this variable in the container configuration and restart to apply it.
**Recommended:** leave it unset; the default is `info`.

Transparent huge pages are a Linux feature that hands out memory in 2 MB pieces. They can make Lodestar's memory use creep up over days, so Lodestar turns them off for itself only.

A custom container variable with key `LODESTAR_THP` and value `allow` turns them back on; removing the variable turns them off again. A change takes effect when the container restarts.
**Recommended:** leave it unset unless you are asked to set it while tracking down a problem.

Blocklist downloads ignore the `HTTPS_PROXY` variable, a setting that sends web requests through a proxy server. They use the container's own route, so a VPN that covers the whole container still applies. **Recommended:** to send them through a VPN, use the gluetun template ([section 3.3](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn)), not a proxy variable.

<!-- nav:start -->
---

[← Appendix A: Answers to check your understanding](appendix-a-answers.md) · [Contents](README.md#contents) · [Glossary →](glossary.md)
<!-- nav:end -->
