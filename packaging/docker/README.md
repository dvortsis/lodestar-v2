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
| `TZ`                   |                   | Time zone for log timestamps.              |

`lodestar --help` lists the rest. Settings can also go in
`/data/config.toml`; environment variables override it.

## Volume, user and permissions

`/data` is the only volume. The image runs as distroless **`nonroot`
(65532:65532)** and ships an empty `/data` owned by that account, so a new
**named volume** is writable out of the box.

To run as a different account, set `user: "<uid>:<gid>"` in compose (or
`--user <uid>:<gid>` for `docker run`) **and** make the data directory writable
by it, which means a bind mount you own:

```sh
mkdir data && sudo chown 1000:1000 data
docker run --user 1000:1000 -v "$PWD/data:/data" ...
```

If the container exits with a permission error creating files in `/data`, the
mounted directory isn't writable by the account the process runs as.

**`PUID`/`PGID` aren't supported yet.** The binary doesn't drop privileges
itself, and the image has no shell to do it, so use `user:` as above.
(Unraid: add `--user 99:100` to *Extra Parameters* until it does.)

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

Back up the whole directory while the container is stopped.

## Health check

`HEALTHCHECK` runs `/lodestar healthcheck` (exec form, no shell) every 30 s.
It requests `/healthz` on `127.0.0.1` at the port of `LODESTAR_HTTP_BIND`, and
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
