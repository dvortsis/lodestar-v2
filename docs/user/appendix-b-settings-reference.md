# Appendix B: Settings reference

Every field in the Lodestar Unraid template in one table. The recommended value is the safe choice for most setups.

| Setting | What it does | Recommended | Notes |
|---|---|---|---|
| Web UI | Port for the web page and API (`8090/tcp`) | `8090` | Never forward it on your router. |
| DHT / peers (UDP) | Port for DHT traffic (`6881/udp`) | `6881` | Forwarding is optional and shows your IP to DHT peers. |
| Peers (TCP) | Reserved for incoming peers (planned) | `6881` | Nothing listens yet. Don't forward it. |
| Data | Index, database and settings (`/data`) | `/mnt/cache/appdata/lodestar` | Use a pool path, not `/mnt/user`. |
| Hot path (optional) | Planned, not used yet | empty | Needs SSD or NVMe if used later. |
| Cold path (optional) | Planned, not used yet | empty | Keep on the cache pool if used later. |
| PUID | User ID Lodestar runs as; it makes Data this user's on start | `99` | Never `0` (root). |
| PGID | Group ID Lodestar runs as | `100` | Never `0` (root group). |
| TZ | Time zone | `Etc/UTC` | Log timestamps are UTC today. |
| Intensity | Crawler effort (`LODESTAR_INTENSITY`) | `balanced` | `aggressive` can overload home routers. |
| DHT port | Gluetun template only (`LODESTAR_DHT_PORT`) | `6881` | Match the port your VPN provider forwards. |

The templates do not include a Log level field. You can optionally add a custom
container variable with key `LODESTAR_LOG` for console logging (`error`, `warn`, `info` or `debug`).
**Recommended:** leave it unset; the default is `info`.

<!-- nav:start -->
---

[← Appendix A: Answers to check your understanding](appendix-a-answers.md) · [Contents](README.md#contents) · [Glossary →](glossary.md)
<!-- nav:end -->
