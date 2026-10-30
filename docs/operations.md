---
format: user-guide
title: Operations Guide
audience: Lodestar operators who can install an app and need setup guidance
sources: docs/architecture.md; docs/owner-decisions.md; crates/lodestar/src; packaging; docs/user
---

# Operations Guide

How to install, run and look after Lodestar. It follows `docs/architecture.md`
§11 and §12. **Status labels:** anything marked **(planned)** is designed but
not built yet; don't rely on it. Unmarked statements describe the code on
`main` today.

## 1. What Exists Today

| Area | Today | Planned |
|---|---|---|
| Binary | `lodestar serve` (default), `lodestar healthcheck`, `lodestar import-bitmagnet --host <host>`, `--version`, `--help` | `reindex`, `backup`, `rollback`, `import`, `vacuum`, `stats`, `companion`, `worker` |
| Settings | `config.toml` and the `LODESTAR_*` variables below | Editing settings in the UI (milestone B2) |
| Packaging | Unraid templates and a systemd unit (`packaging/`) | Docker image and compose file, release tarballs, Windows MSI |
| Workers | — | Worker mode (`LODESTAR_JOIN`, `LODESTAR_TOKEN`, cluster port 8091), Lodestar v0.6+ |

Environment variables the binary reads today:

| Variable | Meaning | Default |
|---|---|---|
| `LODESTAR_DATA_DIR` | Data directory | `data` |
| `LODESTAR_HTTP_BIND` | Web UI and API address | `0.0.0.0:8090` |
| `LODESTAR_DHT_PORT` | Port for the DHT socket | `6881` |
| `LODESTAR_DHT_ENABLED` | `true` or `false` | `true` |
| `LODESTAR_INTENSITY` | `low`, `balanced` or `aggressive` | `balanced` |
| `LODESTAR_SCRAPE_ENABLED` | Tracker and DHT scrapes (§9): `true` or `false` | `true` |
| `LODESTAR_LOG` | `error`, `warn`, `info`, `debug` or a `tracing` filter | `info` |
| `LODESTAR_DHT_SAFE_RESTART` | `true` lets a clean restart skip the six-hour sampling wait (`[dht] safe_restart`, CR-0d) | `false` |
| `LODESTAR_DHT_WITNESS_DIR` | The restart record folder, outside the data directory and never backed up with it. The Docker image and the systemd unit set `/var/lib/lodestar-witness`; the Windows service uses `%ProgramData%\Lodestar-witness` | Native: `$XDG_STATE_HOME/lodestar` (or `~/.local/state/lodestar`) on Linux, `%LOCALAPPDATA%\Lodestar\witness` on Windows |

Everything else is in `config.toml` (`--config <path>`, or
`$LODESTAR_DATA_DIR/config.toml`). Unknown keys are rejected. The variables
take precedence over the file: when both set a value, Lodestar uses the variable.

In Docker, `PUID` and `PGID` choose the user Lodestar runs as. The default,
65532, is the safe choice; the Unraid template uses 99 and 100 (`nobody` and
`users`). Leave them as they are. Never set either to 0: that's root, which
is unsafe, and Lodestar refuses to start with it. `TZ` is accepted, but log
timestamps are UTC today. Details: `packaging/docker/README.md`.

## 2. Installing

### Docker **(planned)**
The multi-arch image (`ghcr.io/dvortsis/lodestar-v2`) and a
`docker-compose.yml` come with the Docker packaging slice. The design: one
container, `/data` as a volume, `8090/tcp` for the web UI, `6881/udp` for the DHT
(`6881/tcp` is reserved for incoming peer connections, planned), a `HEALTHCHECK` that runs
`lodestar healthcheck`.

### Unraid
Templates are in `packaging/unraid/`:

- `lodestar.xml`: the standard container.
- `lodestar-gluetun.xml`: runs through a gluetun VPN container (§4.3).
- `lodestar-worker.xml`: a worker for an existing Lodestar. **(planned;
  requires Lodestar v0.6+.)**

The templates point at the image above, so they work once it is published.
Until Community Applications lists Lodestar, copy the XML file to
`/boot/config/plugins/dockerMan/templates-user/` on the server, then add the
container from **Docker → Add Container → Template**.

Defaults: web UI on 8090, `6881/udp` (and a reserved, unused `6881/tcp`), `PUID=99`, `PGID=100`,
and `/data` mapped to `/mnt/cache/appdata/lodestar`. See §5 for why that is a
pool path.

### systemd (Linux)
See `packaging/systemd/README.md`. In short: install the binary to
`/usr/local/bin/lodestar`, install `lodestar.service`, then
`systemctl enable --now lodestar`. The unit runs as a dynamic user, keeps
state in `/var/lib/lodestar`, runs at lower CPU and I/O priority, and allows
8192 open files (400 metadata sockets at the default intensity, plus
headroom).

For release tarball packaging, see [the Linux package guide](../packaging/linux/README.md).
To build from source, install Node.js 22 or newer and the project's Rust toolchain.
From the repository root, build the frontend before Rust embeds it:

```sh
cd web
npm ci
npm run build
cd ..
cargo build --release -p lodestar
```

Without `web/build`, the Rust build embeds a stub page instead of the application UI.
Rebuild the frontend and then the binary to include the full UI.

### Windows **(planned)**
An MSI that installs a Windows Service (data in `%ProgramData%\Lodestar`,
below-normal priority, firewall rules for the DHT port) and a portable zip.
Nothing is published yet.

## 3. First Run

1. Start Lodestar and open `http://<host>:8090/`.
2. On a fresh data directory it prints a one-time **setup token** to the log
   at warn level:
   `first run: open http://<bind>/ and enter setup token <token> to create the admin password`
   - Docker/Unraid: `docker logs lodestar` (or the container's **Logs** in
     Unraid).
   - systemd: `journalctl -u lodestar | grep "setup token"`.
   - Terminal: the console.
3. Enter the token and choose the admin password. Anyone who can read the log
   can claim a fresh instance, so do this before exposing the port.
4. The token is generated fresh at each start while no admin exists, and is
   not stored. Once the admin password is set, no token is printed or needed.

The crawler starts right away. Early on the library fills slowly because the
routing table is still warming up; the status page and `/metrics` show
progress.

## 4. Networking

### 4.1 Ports
| Port | Use |
|---|---|
| `8090/tcp` | Web UI, API, `/metrics`, `/healthz`. Keep it on your LAN. |
| `6881/udp` | DHT. Forward it on your router for the best reach. |
| `6881/tcp` | Reserved for incoming peer connections **(planned)**. Fetches are outbound only today, so there's nothing to forward yet. |
| `8091/tcp` | Cluster port for workers **(planned)** |

Change the DHT port with `LODESTAR_DHT_PORT`. The web address comes from
`LODESTAR_HTTP_BIND`.

### 4.2 Bridge or host
- **Bridge with port mappings** (the Unraid default). Map UDP explicitly; a
  mapping for TCP alone doesn't carry the DHT.
- **Host networking** gives the DHT the real address and removes the NAT hop.
  Choose it when reach is the goal, and be aware it exposes `8090` on all
  host interfaces. Set `LODESTAR_HTTP_BIND` to a LAN address to limit that.

### 4.3 VPN with gluetun
`lodestar-gluetun.xml` sets `--network=container:gluetun`. Then:

- Publish `8090` on the **gluetun** container (Unraid: edit gluetun, add the
  port), because Lodestar has no network of its own.
- Inbound DHT and peer traffic only works if your VPN provider offers port
  forwarding. Forward a port at the provider, allow it in gluetun with
  `FIREWALL_VPN_INPUT_PORTS`, and set Lodestar's `LODESTAR_DHT_PORT` to the
  same number.
- Without a forwarded port, the crawler still runs and fetches metadata, but
  reaches fewer nodes.
- If gluetun restarts, restart Lodestar afterwards; a container sharing
  another container's network stays attached to the old one.

### 4.4 IPv6
`dht.bind` accepts at most one IPv4 and one IPv6 address in `config.toml`
(for example `["0.0.0.0:6881", "[::]:6881"]`). The default is IPv4 only. In
bridge mode on Docker/Unraid IPv6 needs host networking or a v6-enabled
network.

### 4.5 Home routers and connection tracking
The crawler opens hundreds of short outbound TCP connections at once
(about 400 at `balanced`, about 1000 at `aggressive`). Consumer routers have
small connection-tracking tables. When one fills, the whole household's
network gets slow or drops connections. Start at `low`, raise the intensity
step by step, and watch the router's connection count. The VPN variant avoids
this on the home side because the tunnel carries the load.

## 5. Unraid Storage

- **Put `/data` on a pool, not the array and not `/mnt/user`.** Use
  `/mnt/cache/appdata/lodestar` or the name of your pool
  (`/mnt/<pool>/appdata/lodestar`). Index I/O is random, so it belongs on SSD
  or NVMe.
- **Why not `/mnt/user`:** it goes through Unraid's FUSE layer (`shfs`),
  which is slow for memory-mapped files and risky for SQLite's write-ahead
  log.
- **Split paths (optional).** `/data/hot` (indexes and hot columns) must be on
  SSD/NVMe. `/data/cold` (file-tree blobs, optional raw archive) is written
  continuously as torrents arrive, so on the parity array it keeps disks
  spinning and slows writes. The recommended layout is both on the cache pool,
  with only backups going to the array. **(Split storage is planned; today
  everything lives under `/data` and the two optional mappings are not used
  yet.)**

## 6. Data Directory

Today, under `LODESTAR_DATA_DIR`:

| File | Content |
|---|---|
| `lodestar.db` (+ `-wal`, `-shm`) | SQLite database, the source of truth |
| `index/` | Search index; rebuilt from the database if lost |
| `dht-state.bin` | DHT routing table and node IDs |
| `auth.json` | Admin password hash and session key (mode 0600) |
| `seal.key` | Key for sealed values; **back it up with the database** |
| `config.toml` | Optional settings |
| `import-bitmagnet.cursor` | Resume point of a bitmagnet import |

Swarm-statistics hot columns are plain in-memory atomics. A clean shutdown
snapshots them to `hot/*.col`; after a crash they're rebuilt from the
database at the next start (§9). `cold.db` and the `data/hot`/`data/cold`
split are **planned**.

## 7. Upgrades and Backups

- **Upgrade today:** stop Lodestar, replace the binary (or pull the new image
  tag once images exist), start it. Before upgrading, copy the whole data
  directory while the service is stopped. That is your rollback.
- **Snapshots before store upgrades.** When a new version has to change the
  database's layout (a schema migration), it first saves a copy beside it:
  `lodestar.pre-v<N>.db`. To go back, stop Lodestar and run
  `lodestar rollback`. It lists the snapshots, with each one's age and the
  date it will be deleted. Then run `lodestar rollback <snapshot-filename>` to restore one.
  Replace `<snapshot-filename>` with the filename printed in that list.
  - **Snapshots are deleted 90 days after they are taken.** That is the
    window to roll back an upgrade. For a longer-term copy, make a backup
    (below) and keep it somewhere safe.
  - **A rollback keeps the store it replaced** as
    `lodestar.before-rollback-<time>.db`, so you can undo it with
    `lodestar rollback <that name>`. This copy is also deleted after 90
    days.
  - **Snapshots get the same clean-up as the database.** Every hour while
    it runs, Lodestar also removes from the snapshots (and the copies
    rollbacks keep) the expired quarantine entries (with their hidden
    names), the torrents an abuse report or a filter deleted, and searches
    removed from the search log, so a snapshot can't bring them back.
    API keys, feed tokens, saved searches and collections you deleted go
    too, and keys you revoked are revoked in the snapshots. Other changes
    to your own data, such as an edited note or saved search, stay in a
    snapshot until it is deleted.
  - `lodestar stats` shows how many snapshots there are and their total
    size.
- **`lodestar backup`, using SQLite's online backup (planned).** Until then,
  back up with the service stopped: copy `lodestar.db` (including `-wal`),
  `seal.key`, `auth.json` and `config.toml`. The index can be rebuilt, so it
  can be skipped.
- **Update notices** inside the app are planned. Lodestar sends no telemetry.
- **Importing from bitmagnet:** stop Lodestar and bitmagnet, then run
  `lodestar import-bitmagnet --host <host> --password-file <file>`. It reads
  the bitmagnet database read-only, can be resumed, and accepts `--limit N`.
  - **Connection:** `--port` (5432), `--dbname` (`bitmagnet`) and `--user`
    (`bitmagnet_reader`, the read-only role from
    `docs/operations/imports/bitmagnet-restore.md`).
  - **Password:** from `--password-file` (keep it `chmod 600`) or the
    `LODESTAR_IMPORT_PG_PASSWORD` variable. There's no `--password`, because
    other users on the machine can see a command line.
  - **TLS:** always used and verified, against the system's certificates or
    the ones in `--ca-file`. To skip TLS for a database on the same machine
    (a loopback address, or a socket folder such as
    `--host /var/run/postgresql`), add `--allow-plaintext-loopback`.
  - **A database that's still being written is refused.** Stop bitmagnet, or
    import a restored copy.

  Start Lodestar again afterwards; it indexes the imported torrents and
  re-fetches incomplete file lists.

## 8. Troubleshooting

**Check health.** `lodestar healthcheck` exits 0 when the local instance
answers `/healthz`, and 1 with a reason otherwise (for example
`stopped: dht`).

**Nothing is discovered.**
1. Check the log for DHT bootstrap errors. DNS or outbound UDP blocked?
2. Confirm `LODESTAR_DHT_ENABLED` isn't `false`.
3. Check that `6881/udp` is mapped and forwarded (§4). Without inbound
   traffic the crawler still works, but slowly.
4. Under a VPN, see §4.3.

**The web UI doesn't load.** Check `LODESTAR_HTTP_BIND`, the port mapping, and
that nothing else uses 8090. Behind a TLS reverse proxy, set
`http.secure_cookies = true` in `config.toml`.

**Slow disk, or SQLite "database is locked".** Check the data path. If it is
under `/mnt/user` on Unraid, move it to a pool path (§5). The app is meant to
warn when it finds data on a FUSE or network filesystem; **that warning is
planned and not in the binary yet**, so check the path yourself.

**The router or network gets slow.** See §4.5; lower `LODESTAR_INTENSITY`.

**"Too many open files".** Under systemd raise `LimitNOFILE`; in Docker raise
`--ulimit nofile`. About 1000 sockets are used at `aggressive`.

**Lost the setup token.** While no admin exists, every start prints a new
token, so restart the service and read the log again.

## 9. Swarm Health

Swarm health is how Lodestar learns whether a torrent is alive: how many seeders and leechers it has, how many times it has completed, and how many trackers know about it. The numbers drive the seeders and trackers filters, the seeders and trackers sorts, and the ranking penalty for suspect swarms (architecture §3, §6, §15).

### What scraping is, and what it isn't

- A scrape asks a tracker for the seeders, leechers and completed counts of up to 74 hashes at a time (fewer over HTTP).
- A scrape announces nothing. Our IP address never enters a swarm's peer list, so nobody downloading that torrent sees us.
- The tracker does learn which hashes we asked about. That's the cost of scraping, and it's why private torrents are never scraped.
- Torrents flagged `private`, and keys that only private sources reported (`private_source`), are never scraped, by tracker or by DHT. A public lookup would leak the hash (BEP 27, architecture §18.7).
- Lodestar keeps counts only. It never keeps peer addresses or the bloom filters a DHT scrape returns (architecture §18.7).

### Reading the numbers

- **Seeders and leechers** are the highest counts any tracker reported in the last round that got an answer. The fake-seeding check below can set one tracker aside.
- **The DHT estimate stands in only when no tracker reports.** It's never mixed with tracker counts. When it's used, the numbers are marked "≈".
- **"—" means never scraped.** It isn't zero. A real `0` means a tracker or the DHT answered and said there is nobody.
- **"Trackers reporting" and "checked":**
  - The detail view shows "n of m trackers". m is how many trackers answered in the last round that got an answer, and n is how many of those knew the torrent. A tracker that didn't answer is in neither number.
  - Only a tracker that answers with seeders + leechers + completed > 0 counts as reporting.
  - The reason is that opentracker-style trackers answer with zeros for hashes they've never seen. Counting those would make every torrent look known.
  - So "reporting" is a sampled signal that ages. The detail view shows when it was last refreshed.
- **"≈"** marks counts from the DHT estimate (see BEP 33 below).

### The `[scrape]` config section

All keys are optional. This example shows the defaults:

```toml
[scrape]
enabled = true

# Public UDP trackers to scrape. This is the built-in default list.
trackers = [
  "udp://tracker.opentrackr.org:1337/announce",
  "udp://open.demonii.com:1337/announce",
  "udp://open.stealth.si:80/announce",
  "udp://tracker.torrent.eu.org:451/announce",
  "udp://exodus.desync.com:6969/announce",
  "udp://explodie.org:6969/announce",
  "udp://tracker.theoks.net:6969/announce",
  "udp://opentracker.io:6969/announce",
  "udp://tracker.dler.org:6969/announce",
  "udp://tracker.tiny-vps.com:6969/announce",
]

# BEP 33 estimates from the DHT, for new, viewed and rediscovered torrents.
dht = true

# Seconds between UDP requests to each tracker. Unset follows the DHT
# intensity: Low 5, Balanced 2, Aggressive 1.
# interval_secs = 2

# Seconds between HTTP requests to each tracker. Unset is 3x the UDP interval.
# http_interval_secs = 6
```

- `enabled = false` turns off all scraping, tracker and DHT. If set, the environment variable `LODESTAR_SCRAPE_ENABLED` takes precedence over it.
- `dht = false` keeps tracker scraping and drops the BEP 33 estimates.
- `trackers` accepts UDP (BEP 15) and HTTP or HTTPS (BEP 48) URLs.
  - Unknown keys in `[scrape]` are startup errors, so typos surface.
  - So is a tracker URL that can't be scraped, such as `wss://` (WebTorrent) or an HTTP URL whose last path segment doesn't start with `announce`.
  - The same tracker listed twice counts once. Lodestar compares the canonical form: `udp://host:port` for UDP, with any BEP 41 path dropped, and the normalised URL without its fragment for HTTP.
- `interval_secs` and `http_interval_secs` are per tracker. A tracker's own `min_request_interval` can only slow it down further. It never speeds it up (see tracker health below).
- Scrape errors never include the tracker URL, because it may hold a passkey.

### The schedule

Every torrent that can be scraped is in one of a few lanes. The scheduler serves them in turn (deficit round robin), so a flood in one lane can't starve the others.

- **New torrents** are scraped once, soon after they're stored.
- **Viewed and top-of-search torrents** are scraped promptly. A torrent you open is refreshed if its data is older than 30 minutes.
- **Rediscovered torrents** get priority. When a known hash shows up in the DHT again, that's a free liveness signal (architecture §5), so its scrape moves up, unless it was scraped in the last 6 hours. Popular torrents are rediscovered constantly.
- **Alive swarms** (some seeder or leecher) are rescraped every 6 hours. While the counts stay steady, the gap doubles, up to 3 days. It resets to 6 hours when a count moves by a fifth, or by 2 peers or more.
- **Dead swarms** (nothing reported) are rescraped every 2 days, doubling to 30 days.
- **Unreachable rounds**, where no tracker and no DHT answer arrived, retry after 1 hour, doubling with each unanswered round in a row, up to 1 day. The last known counts stay as they were, since a tracker outage isn't a dead swarm. The first answer resets the retry.
- If every tracker is backing off or banned, no rounds run at all. Nothing is marked unreachable for a wait of Lodestar's own making.
- Every interval has ±10% jitter, so a batch of torrents stored together doesn't come due together.

### Throughput at Balanced

- Each tracker gets one UDP request every 2 seconds. A request carries up to 74 hashes.
- That's 37 torrents a second per tracker, or about 3.2 million torrent-scrapes a day (37 × 86,400).
- The limit is per tracker, so adding trackers adds coverage ("n of m trackers") but not more distinct torrents per second.
- A full sweep of 10 million hashes against 20 trackers would take about 2.7 million UDP requests per pass. Trackers ban clients that do that, so Lodestar doesn't try (architecture §15). The schedule above spends the budget where it matters: new, viewed and rediscovered torrents first, then a slow decay over the rest.
- Low (one request every 5 s) and Aggressive (one every 1 s) scale this by 0.4 and 2.

### Tracker health

Each tracker is in one of four states:

| State | Meaning | What Lodestar does |
|---|---|---|
| Ok | answering | scrapes at the configured interval |
| Backoff | a recent failure (timeout or error) | waits 1 minute, doubling on each further failure |
| Down | 5 failures in a row | skips the tracker, except for a probe once an hour |
| Banned | a refusal that mentions a rate limit or ban, or HTTP 429 or 403 | stays away for 24 hours, doubling on repeat to a week |

- A tracker's `min_request_interval` (the HTTP scrape response may carry one) only ever slows requests. It's honoured up to a day.
- A slow tracker isn't dropped. If it can't take a whole round's hashes before the round closes, it gets the ones it can (a partial job), and the rest of the round goes to the other trackers. Answers that still arrive after a round closed are counted as `late` in the metrics and dropped.
- Health isn't saved. In M4 every tracker starts as Ok after a restart.
- The status page lists each tracker with its state, so a banned tracker is visible rather than silently missing.

### Fake seeding

Some trackers inflate their counts. Lodestar checks swarms against each other:

- A swarm is flagged **suspect** when one tracker claims at least 1,000 seeders and at least 50 times more than every other source. The other sources are the other trackers and, if two or more DHT nodes answered, the DHT estimate.
- For a suspect swarm, the counts come from the other sources, and the detail view says why.
- It is a ranking penalty, not a deletion. The torrent stays searchable.
- It's kept apart from the stored junk score, because swarms change.

### Routing, proxies and VPNs

- Scrapes leave from the same bind address as the DHT (`dht.bind`). If the crawler runs through a VPN, so do the scrapes.
- Subscribed IP blocklists cover trackers as well. A tracker whose every address is listed is never contacted, and the status page shows it backing off.
- HTTP scrapes ignore `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY`. They go out the bound interface like everything else the crawler does.
- Trackers see the IP the scrape leaves from. On a home connection that's your own address, and public trackers may log it. A VPN is advised, and in Docker `network_mode: service:gluetun` covers the scraper too.
- If the status page shows trackers as banned, lengthen `interval_secs`.

### BEP 33: the DHT estimate

- A DHT scrape asks nodes near the hash for their seed and peer bloom filters, merges the filters of the 8 closest nodes that have any, and estimates the counts from the merged filter.
- It's an estimate, so it's shown as "≈". It is accurate for small swarms and saturates near 7,800.
- Zero responders means no node near the hash stores it. That reads as "unknown", not "dead".
- Only the two counts are kept. The filters, and any peers learned along the way, are dropped when the lookup ends.
- DHT scrapes are limited to about 10% of the query budget, so they serve new, viewed and rediscovered torrents. The decay schedule uses them only for a swarm that the DHT alone showed alive, since the trackers would call it dead. They don't feed the metadata fetcher.

### Hot columns

- Seeders, leechers, trackers, the DHT estimate and last seen live in in-memory arrays, so searches can filter and sort on them with no locks. They cost about 20 bytes per torrent of RAM, about 200 MB at 10 million torrents, which counts against the 1 GB budget.
- On a clean shutdown they're snapshotted to `data/hot/*.col` with a stamp.
- After a crash, or if the stamp doesn't match the database, they're rebuilt from SQLite at startup. That takes longer on big corpora, so expect a slower start.
- SQLite stays the source of truth. Nothing is lost if the snapshot is deleted.

### Watching it

- The status page has a "Swarm health" section: scraped, alive and suspect counts and a table of trackers with their state, counts and last answer. It's hidden when scraping is off.
- Prometheus metrics cover scrape requests by tracker and outcome and the DHT scrape counters. Their names arrive with the binary wiring, so check `/metrics` on your build.
