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

## Settings → Sources

These are in the web app, under **Settings → Sources**, not in the template. Each source has its own row. See [section 5.4](05-use-lodestar.md#54-choose-where-lodestar-finds-torrents-settings--sources).

| Setting | What it does | Recommended | Notes |
|---|---|---|---|
| On | Turns the source on or off | On for **Mainline DHT**; off for **External** and **Account** sources unless you need them | Saved at once, with no Undo. The row shows **Applies at next start**: the source starts or stops at the next restart. |
| Priority | Share of fetch work (**Low**, **Normal**, **High**, **Maximum**) | `Normal` | Saved and applied at once, with no Undo. |
| Setup | Accepting what a source does and the sites it contacts, adding account details, or typing your password | Not available yet | A source marked **Needs setup** stays off. |

The templates do not include a Log level field. You can optionally add a custom
container variable with key `LODESTAR_LOG` to set how much Lodestar logs on every output
(`error`, `warn`, `info` or `debug`). While it is set, Logging verbosity in Settings can't be changed.
**Recommended:** leave it unset; the default is `info`.

Transparent huge pages are a Linux feature that hands out memory in 2 MB pieces. They can make Lodestar's memory use creep up over days, so Lodestar turns them off for itself only.

A custom container variable with key `LODESTAR_THP` and value `allow` turns them back on; removing the variable turns them off again. A change takes effect when the container restarts.
**Recommended:** leave it unset unless you are asked to set it while tracking down a problem.

Blocklist downloads ignore the `HTTPS_PROXY` variable, a setting that sends web requests through a proxy server. They use the container's own route, so a VPN that covers the whole container still applies. **Recommended:** to send them through a VPN, use the gluetun template ([section 3.3](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn)), not a proxy variable.

<!-- nav:start -->
---

[← Appendix A: Answers to check your understanding](appendix-a-answers.md) · [Contents](README.md#contents) · [Glossary →](glossary.md)
<!-- nav:end -->
