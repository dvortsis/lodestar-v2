# Operations guide (draft)

How to install, run and look after Lodestar. It follows `docs/architecture.md`
§11 and §12. **Status labels:** anything marked **(planned)** is designed but
not built yet; don't rely on it. Unmarked statements describe the code on
`main` today.

## 1. What exists today

| Area | Today | Planned |
|---|---|---|
| Binary | `lodestar serve` (default), `lodestar healthcheck`, `lodestar import-bitmagnet <postgres-url>`, `--version`, `--help` | `reindex`, `backup`, `rollback`, `import`, `vacuum`, `stats`, `companion`, `worker` |
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
| `LODESTAR_LOG` | `error`, `warn`, `info`, `debug` or a `tracing` filter | `info` |

Everything else is in `config.toml` (`--config <path>`, or
`$LODESTAR_DATA_DIR/config.toml`). Unknown keys are rejected. The variables
override the file. `PUID`, `PGID` and `TZ` are handled by the container
image, which doesn't exist yet **(planned)**; the Unraid templates already
expose them.

## 2. Installing

### Docker **(planned)**
The multi-arch image (`ghcr.io/dvortsis/lodestar-v2`) and a
`docker-compose.yml` come with the Docker packaging slice. The design: one
container, `/data` as a volume, `8090/tcp` for the web UI, `6881/udp` and
`6881/tcp` for the DHT and peers, a `HEALTHCHECK` that runs
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

Defaults: web UI on 8090, `6881/udp` and `6881/tcp`, `PUID=99`, `PGID=100`,
and `/data` mapped to `/mnt/cache/appdata/lodestar`. See §5 for why that is a
pool path.

### systemd (Linux)
See `packaging/systemd/README.md`. In short: install the binary to
`/usr/local/bin/lodestar`, install `lodestar.service`, then
`systemctl enable --now lodestar`. The unit runs as a dynamic user, keeps
state in `/var/lib/lodestar`, runs at lower CPU and I/O priority, and allows
8192 open files (400 metadata sockets at the default intensity, plus
headroom).

Release tarballs are **(planned)**. Until then, build with
`cargo build --release -p lodestar`.

### Windows **(planned)**
An MSI that installs a Windows Service (data in `%ProgramData%\Lodestar`,
below-normal priority, firewall rules for the DHT port) and a portable zip.
Nothing is published yet.

## 3. First run

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
| `6881/tcp` | Peer connections. Forward it together with the UDP port. |
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

## 5. Unraid storage

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

## 6. Data directory

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

`data/hot/*.col` (memory-mapped statistics columns) and `cold.db` are in the
design (§4, §11) but **not created yet (planned)**.

## 7. Upgrades and backups

- **Upgrade today:** stop Lodestar, replace the binary (or pull the new image
  tag once images exist), start it. Before upgrading, copy the whole data
  directory while the service is stopped. That is your rollback.
- **Automatic snapshot before schema migrations and `lodestar rollback`
  (planned).**
- **`lodestar backup`, using SQLite's online backup (planned).** Until then,
  back up with the service stopped: copy `lodestar.db` (including `-wal`),
  `seal.key`, `auth.json` and `config.toml`. The index can be rebuilt, so it
  can be skipped.
- **Update notices** inside the app are planned. Lodestar sends no telemetry.
- **Importing from bitmagnet:** stop Lodestar, then run
  `lodestar import-bitmagnet postgres://user:pass@host/bitmagnet`. It reads
  the bitmagnet database, can be resumed, and accepts `--limit N`. Start
  Lodestar again afterwards; it indexes the imported torrents and re-fetches
  incomplete file lists.

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
