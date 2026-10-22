# Docker

One image, `ghcr.io/dvortsis/lodestar-v2`: a static musl `lodestar` (with the
web UI embedded) on `gcr.io/distroless/static-debian12:nonroot`, for
`linux/amd64` and `linux/arm64`. There is no shell in the image.

## Run

```sh
docker compose up -d          # from the repository root: docker-compose.yml
docker compose logs lodestar  # the first-run setup token is in the log
```

Then open <http://localhost:8090> and enter the token to create the admin
password. Or with plain `docker`:

```sh
docker run -d --name lodestar --restart unless-stopped \
  -p 8090:8090 -p 6881:6881/udp -p 6881:6881/tcp \
  -v lodestar-data:/data \
  ghcr.io/dvortsis/lodestar-v2:latest
```

## Tags

| Tag            | Moves when                          |
| -------------- | ----------------------------------- |
| `x.y.z`        | never (one per `vX.Y.Z` git tag)    |
| `x.y`          | each patch release of that minor    |
| `latest`       | each stable release                 |
| `nightly`      | every day, built from `main`        |

Pin `x.y.z` for reproducible deployments. `nightly` is untested between
releases.

## Ports

| Port       | Use                                                              |
| ---------- | ---------------------------------------------------------------- |
| `8090/tcp` | Web UI, API, `/healthz`, `/metrics`.                             |
| `6881/udp` | DHT. Forward it on your router for the best reach.               |
| `6881/tcp` | Reserved for incoming peer connections; nothing listens yet.     |

### Bridge or host networking

The compose file uses **bridge networking** with explicit mappings: it works
everywhere and keeps the container isolated. Docker NATs the UDP traffic, so
other DHT nodes can only reach you if the host's `6881/udp` is forwarded.

On Linux, **`network_mode: host`** skips that NAT and gives the best DHT
reachability. Remove the `ports:` block and uncomment `network_mode: host`.
The container then listens on host ports 8090 and 6881; change them with
`LODESTAR_HTTP_BIND` and `LODESTAR_DHT_PORT`. Host mode isn't available on
Docker Desktop (macOS, Windows).

### IPv6

Docker's default bridge is IPv4-only. For IPv6 in bridge mode, enable it on
the daemon (`"ipv6": true` and `fixed-cidr-v6` in `daemon.json`) or give the
compose project a network with `enable_ipv6: true` and an IPv6 subnet. With
`network_mode: host`, the container uses the host's addresses.

## Environment

| Variable               | Image default     | Meaning                                    |
| ---------------------- | ----------------- | ------------------------------------------ |
| `LODESTAR_DATA_DIR`    | `/data`           | Where all state lives.                     |
| `LODESTAR_HTTP_BIND`   | `0.0.0.0:8090`    | Web/API listen address (`ip:port`).        |
| `LODESTAR_DHT_PORT`    | `6881`            | DHT port (UDP).                            |
| `LODESTAR_DHT_ENABLED` | `true`            | `false` turns the DHT off.                 |
| `LODESTAR_INTENSITY`   | `balanced`        | `low`, `balanced` or `aggressive`.         |
| `LODESTAR_LOG`         | `info`            | Log filter (`error`, `warn`, `info`, `debug`). |
| `PUID`                 | `65532`           | User id the server runs as (see below).    |
| `PGID`                 | `65532`           | Group id the server runs as.               |
| `LODESTAR_ALLOW_ROOT`  |                   | `1` permits `PUID=0`/`PGID=0`. Don't.      |
| `TZ`                   |                   | Accepted, but log timestamps are UTC today. |

Settings can also go in `/data/config.toml`; environment variables override
it. Because the image sets `LODESTAR_HTTP_BIND`, it overrides `[http] bind` in
`config.toml`: change the web port with the variable (and the port mapping),
not in the file.

## Volume, user and permissions

`/data` is the only volume. The container starts as root only long enough for
the binary to apply **`PUID`/`PGID`** (there's no shell for an entrypoint
script to do it). Before it reads `config.toml`, binds a port or starts a
thread, it:

1. checks `PUID`/`PGID`: plain numbers, default `65532` (distroless
   `nonroot`). `0` is refused unless `LODESTAR_ALLOW_ROOT=1`;
2. makes `/data` theirs. If `/data` itself already belongs to `PUID:PGID`,
   nothing is touched. Otherwise the whole tree is handed over, without
   following symlinks; hard-linked files and device, fifo or socket entries
   are left alone. A symlinked `/data` is refused. The log says what changed;
3. clears the supplementary groups, then sets the group and user ids, and
   checks that root can't be regained (`setuid(0)` fails and no capabilities
   remain). If any step fails, it doesn't start.

So a new **named volume** works out of the box, and so does a **bind mount**
owned by anyone: it's handed to `PUID:PGID` on the first start.

```sh
docker run -e PUID=1000 -e PGID=1000 -v "$PWD/data:/data" ...
```

Unraid's template sets `PUID=99`/`PGID=100` (`nobody:users`).

**`--user` still works** (compose `user:`). The container then starts
unprivileged, `PUID`/`PGID` are ignored (the log says so if they differ), and
nothing is chowned, so the data directory must already be writable by that
account:

```sh
mkdir data && sudo chown 1000:1000 data
docker run --user 1000:1000 -v "$PWD/data:/data" ...
```

If the container exits with a permission error creating files in `/data`, the
mounted directory isn't writable by the account the process runs as.

`config.toml` can't move the data directory for `PUID`/`PGID`: ownership is
checked on `LODESTAR_DATA_DIR` (`/data` in the image), which overrides the
file anyway.

Keep `/data` on local SSD/NVMe storage; the index does random I/O and the
database is SQLite. Avoid network shares and FUSE filesystems.

## Data layout

What the code writes under `/data` today:

| Path             | Contents                                                   |
| ---------------- | ---------------------------------------------------------- |
| `lodestar.db`    | The SQLite database (plus `-wal`/`-shm` while running).    |
| `index/`         | The search index; rebuildable from the database.           |
| `dht-state.bin`  | DHT routing-table state, so restarts re-join quickly.      |
| `auth.json`      | The admin credentials (hashed).                            |
| `seal.key`       | The key that seals stored secrets. Back it up with the rest. |
| `config.toml`    | Optional settings (you create it).                         |
| `import-bitmagnet.cursor` | Resume point of `lodestar import-bitmagnet`.      |

Back up the whole directory while the container is stopped.

The compose file sets `stop_grace_period: 60s`: shutdown flushes the index and
database, which can take longer than Docker's default 10 s. Keep that (or
`docker stop --time 60`) if you run it another way.

## Health check

`HEALTHCHECK` runs `/lodestar healthcheck` (exec form, no shell) every 30 s.
It needs no privilege: started as root, it drops to `PUID`/`PGID` first, like
the server (without touching `/data`). It requests `/healthz` on `127.0.0.1` at the port of `LODESTAR_HTTP_BIND`, and
exits 0 on a `200`. Check it with
`docker inspect --format '{{.State.Health.Status}}' lodestar`.

## Build

The build context is the repository root:

```sh
docker buildx build -f packaging/docker/Dockerfile -t lodestar .
# both architectures
docker buildx build --platform linux/amd64,linux/arm64 -f packaging/docker/Dockerfile .
```

Stages: Node builds `web/` → Rust builds `lodestar` for musl → distroless. The
Rust stage always runs on the build machine; `linux/arm64` is
**cross-compiled** with `cargo-zigbuild` (zig as the C compiler and linker), so
nothing runs under QEMU. Build arguments `VERSION`, `REVISION` and `CREATED`
fill the OCI labels. BuildKit cache mounts keep the cargo registry and `target/`
between builds.

`.github/workflows/release.yml` builds and smoke-tests the image on every
relevant pull request. Version tags and the daily run also push it.
