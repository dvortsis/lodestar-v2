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
| `PUID`                 | `65532`           | User ID Lodestar runs as. See below.       |
| `PGID`                 | `65532`           | Group ID Lodestar runs as. See below.      |
| `LODESTAR_ALLOW_ROOT`  |                   | Leave unset. `1` allows running as root, which is unsafe. |
| `TZ`                   |                   | Accepted, but log timestamps are UTC today. |

Settings can also go in `/data/config.toml`; environment variables override
it. Because the image sets `LODESTAR_HTTP_BIND`, it overrides `[http] bind` in
`config.toml`: change the web port with the variable (and the port mapping),
not in the file.

## Volume, user and permissions

`/data` is the only folder Lodestar keeps. Everything in it survives updates;
everything else in the container is thrown away when it's rebuilt.

Lodestar runs as an ordinary user, not as root (the all-powerful admin
account). That limits the damage if something ever goes wrong. Two settings
choose that user:

- **`PUID`**: the user ID. Leave the default, `65532`, unless you need the
  files in `/data` to belong to a particular account on your server.
- **`PGID`**: the group ID. Default `65532`.

On Unraid, the template sets `PUID=99` and `PGID=100` (Unraid's `nobody` and
`users`). Leave those as they are.

To use your own account, find its IDs by running `id` on the server, then set
them:

```sh
docker run -e PUID=<your-uid> -e PGID=<your-gid> -v "$PWD/data:/data" ghcr.io/dvortsis/lodestar-v2:latest
```

- `<your-uid>`, `<your-gid>`: the `uid=` and `gid=` numbers `id` prints, for
  example `1000`.

On every start, Lodestar makes the data folder belong to that user, then gives
up its admin rights for good. If the data folder already belongs to that user,
nothing is changed. The log says what it did.

> [!CAUTION]
> Don't set `PUID` or `PGID` to `0`. That's root, and Lodestar refuses to start
> with it unless you also set `LODESTAR_ALLOW_ROOT=1`. Running as root means a
> bug could change anything on the server. Use any other ID instead.

<details><summary>What exactly happens at start-up</summary>

The container starts as root only so Lodestar can switch users; the image has
no shell for a start-up script. Before it reads `config.toml`, opens a port or
starts any work, Lodestar:

1. checks `PUID` and `PGID` are plain numbers, and stops if they aren't;
2. hands `/data` to `PUID:PGID`, if it belongs to someone else. It never
   follows links out of the folder. It refuses a `/data` that is itself a
   link, and it leaves hard-linked files and device files alone;
3. drops root, then checks it can't get root back. If any step fails, it
   stops instead of running.

The ownership check uses `LODESTAR_DATA_DIR` (`/data` in the image), not a
data folder set in `config.toml`.

</details>

### Running with `--user` (advanced)

You can also choose the user with Docker's `--user` option (compose: `user:`).
Lodestar then never has root, ignores `PUID` and `PGID`, and doesn't change any
file owners. So you must make the data folder writable by that user first:

```sh
sudo chown <your-uid>:<your-gid> data
```

```sh
docker run --user <your-uid>:<your-gid> -v "$PWD/data:/data" ghcr.io/dvortsis/lodestar-v2:latest
```

If the container stops with a permission error about `/data`, the folder isn't
writable by the user Lodestar runs as. Remove `--user` and set `PUID`/`PGID`
instead, or fix the folder's owner as above.

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
It doesn't need root: like the server, it switches to `PUID`/`PGID` first
(without touching `/data`). It requests `/healthz` on `127.0.0.1` at the port of `LODESTAR_HTTP_BIND`, and
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
