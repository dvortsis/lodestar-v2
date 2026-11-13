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

**Published release: v0.5.1.** The [release setup guide](user/README.md) covers it step by step. This page also describes what's on `main`, which is ahead of that release. Where a feature needs a newer version than v0.5.1, the text says **(newer than v0.5.1)**. On `:latest` or a v0.5.1 install, a command marked that way is "unrecognized".

## 1. What Exists Today

Everything below is built on `main`. Anything still missing is marked **(planned)**.

| Area | What you get |
|---|---|
| Binary | One `lodestar` program with the web UI inside. Its commands are in [§10](#10-the-command-line). |
| Docker | An image for `amd64` and `arm64`, a Compose file and three Unraid templates (§2). |
| Linux | Tarballs for `x86_64` and `aarch64`, with a systemd unit (§2). |
| Windows | An `x86_64` MSI that installs a service, and a portable ZIP (§2). |
| Settings | The app's **Settings** pages, `config.toml`, the `LODESTAR_*` variables below, and `lodestar settings` for when the app can't be reached **(newer than v0.5.1)** (§8, §10). |
| Workers **(newer than v0.5.1)** | `lodestar worker` and the Unraid worker template (it says it requires Lodestar v0.6 or later). The main Lodestar accepts workers only when `[cluster] enabled = true` is set in `config.toml`; it is off by default, and then nothing listens on `8091/tcp`. |
| Not built yet | **(planned)**: a Community Applications listing; incoming peer connections on `6881/tcp`; a Settings screen for the web address; a Settings screen for worker join tokens; and updates that install by themselves on Windows (§7). |

Common environment variables (the update settings have their own `LODESTAR_UPDATES*` family; `lodestar --help` lists the main ones):

| Variable | Meaning | Default |
|---|---|---|
| `LODESTAR_DATA_DIR` | Data directory | `data` |
| `LODESTAR_HTTP_BIND` | Web UI and API address | `0.0.0.0:8090` |
| `LODESTAR_DHT_PORT` | Port for the DHT socket | `6881` |
| `LODESTAR_DHT_ENABLED` | `true` or `false` | `true` |
| `LODESTAR_INTENSITY` | `low`, `balanced` or `aggressive` | `balanced` |
| `LODESTAR_SCRAPE_ENABLED` | Tracker and DHT scrapes (§9): `true` or `false` | `true` |
| `LODESTAR_SECURE_COOKIES` | `true` when Lodestar sits behind a TLS reverse proxy (same as `http.secure_cookies`) | `false` |
| `LODESTAR_LOG` | `error`, `warn`, `info`, `debug` or a `tracing` filter. While it's set, **Settings → Logging** is read-only; leave it unset to change logging in the app | `info` |
| `LODESTAR_DHT_SAFE_RESTART` | Development only; unavailable in v0.5.1. `true` lets a clean restart skip the six-hour sampling wait (`[dht] safe_restart`, CR-0d) | `false` |
| `LODESTAR_DHT_WITNESS_DIR` | Development only; unavailable in v0.5.1. The restart record folder, outside the data directory and never backed up with it. The Docker image and the systemd unit set `/var/lib/lodestar-witness`; the Windows service uses `%ProgramData%\Lodestar-witness` | Native: `$XDG_STATE_HOME/lodestar` (or `~/.local/state/lodestar`) on Linux, `%LOCALAPPDATA%\Lodestar\witness` on Windows |

Everything else is in `config.toml` (`--config <path>`, or
`$LODESTAR_DATA_DIR/config.toml`). Unknown keys are rejected. The variables
take precedence over the file: when both set a value, Lodestar uses the variable.

In Docker, `PUID` and `PGID` choose the user Lodestar runs as. The default,
65532, is the safe choice; the Unraid template uses 99 and 100 (`nobody` and
`users`). Leave them as they are. Never set either to 0: that's root, which
is unsafe, and Lodestar refuses to start with it. `TZ` is accepted, but log
timestamps are UTC today. Details: `packaging/docker/README.md`.

## 2. Installing

### Docker

The image is `ghcr.io/dvortsis/lodestar-v2`, for `amd64` and `arm64`. Pin a
version tag such as `:0.5.1` so an update happens when you choose; `:latest`
follows each stable release and `:nightly` is built daily from `main` and isn't
tested between releases.

- **Volume:** `/data` holds everything Lodestar saves (§6). Map it to local
  disk (an SSD if you have one), not a network share.
- **Ports:** `8090/tcp` for the web UI and `6881/udp` for the DHT. The
  `6881/tcp` mapping is reserved; metadata fetches are outbound.
- **Compose file:** `docker-compose.yml` in the repository root. Run
  `docker compose up -d`, then `docker compose logs lodestar` for the setup
  token (§3). The container's own health check runs `lodestar healthcheck`.
- **Stopping:** give the container 60 seconds to stop (`stop_grace_period: 60s`
  in Compose). Lodestar saves its index and database on the way out.

Follow [Chapter 2](user/02-install-lodestar-on-unraid-and-docker.md) for the
step-by-step setup, and `packaging/docker/README.md` for every tag and option.

### Unraid
Templates are in `packaging/unraid/`:

- `lodestar.xml`: the standard container.
- `lodestar-gluetun.xml`: runs through a gluetun VPN container (§4.3).
- `lodestar-worker.xml` **(newer than v0.5.1; requires Lodestar v0.6 or later)**:
  a worker for an existing Lodestar. It asks for the main instance's address and
  a one-time join token. The main instance must have `[cluster] enabled = true`.
  A Settings screen for tokens is **(planned)**; today a token comes from the
  API: `POST /api/v1/workers/tokens` from a signed-in admin browser session (API keys are refused), with a JSON body holding the worker's `name` and your admin `password`.
  See `lodestar worker` in §10.

The templates default to `latest`. Set **Repository** to a version tag such as
`ghcr.io/dvortsis/lodestar-v2:0.5.1` to stay on that version.
Until Community Applications lists Lodestar **(planned)**, copy the XML file to
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

### Windows

Two downloads, both `x86_64`. Check each against the `SHA256SUMS` file on the
release page before you run it (`Get-FileHash <file> -Algorithm SHA256`).

| Download | Choose it when |
|---|---|
| `lodestar-<version>-x86_64.msi` | You want Lodestar to run like any other app. **Recommended.** |
| `lodestar-<version>-x86_64-windows-portable.zip` | You want to try it, or run it from a folder. Delete the folder to remove it. |

The **MSI**:

- installs `lodestar.exe` in `C:\Program Files\Lodestar` and keeps data in
  `C:\ProgramData\Lodestar`;
- registers a Windows service named **Lodestar**. It runs as its own account,
  `NT SERVICE\Lodestar`, starts a little after Windows boots, and restarts if
  it fails;
- limits the data folder to SYSTEM, Administrators and that account;
- allows `6881/udp` through the firewall on **private networks only**;
- adds a Start-menu shortcut to `http://localhost:8090/`;
- has two options, both **off** by default: `WEB_FIREWALL=1` lets other PCs on
  your network open the web UI (`8090/tcp`), and `DEFENDER_EXCLUSION=1` stops
  Windows Defender scanning the data folder. The exclusion makes Lodestar
  faster but nothing in that folder is scanned, so leave it off if you share
  the PC or aren't sure;
- keeps your data when you uninstall, unless you pass `REMOVE_DATA=1`.

The setup token is in `C:\ProgramData\Lodestar\logs\lodestar.log`; only
Administrators can read it. The installer and `lodestar.exe` aren't
Authenticode-signed, so SmartScreen may warn: if the checksum matches, choose
**More info → Run anyway**.

Silent install and uninstall, in an administrator PowerShell window:

```powershell
msiexec /i lodestar-<version>-x86_64.msi /qn            # defaults: both options off
msiexec /x lodestar-<version>-x86_64.msi /qn            # uninstall, keep data
msiexec /x lodestar-<version>-x86_64.msi /qn REMOVE_DATA=1   # uninstall and delete data
```

The **portable ZIP** runs from a folder as the user who unpacked it: no
service, no firewall rules, no helper. Unpack it under your own user folder
(not the top of a drive, a shared drive or OneDrive) and start
`Start Lodestar.cmd`, which keeps the data in the `data` folder beside it.

**Commands from §10 on Windows.** Stop the service first
(`Stop-Service Lodestar`) for any command marked "server stopped". Then, in an
administrator PowerShell window (only Administrators can read the data
folder), tell the command where the data is and run it. The MSI:

```powershell
$env:LODESTAR_DATA_DIR = 'C:\ProgramData\Lodestar'
& 'C:\Program Files\Lodestar\lodestar.exe' stats
```

For the portable ZIP, set `LODESTAR_DATA_DIR` to its `data` folder and run the
`lodestar.exe` beside `Start Lodestar.cmd`. Without the variable the command
looks for a `data` folder in the folder you're in, and stops with "no store".

Follow [Chapter 4](user/04-install-on-windows-and-linux.md) for the guided
steps, and `packaging/windows/README.md` for every option.

## 3. First Run

1. Start Lodestar and open `http://<host>:8090/`.
2. On a fresh data directory it prints a one-time **setup token** to the log
   at warn level, with the address to open:
   `First run: open Lodestar in a browser and enter setup token <token> address="http://<this host's address>:8090/"`
   Use your host's real address in place of the placeholder; the port is the one in `LODESTAR_HTTP_BIND`. In Docker or Unraid that is the port inside the container; open the host port you mapped to it.
   - Docker/Unraid: `docker logs lodestar` (or the container's **Logs** in
     Unraid).
   - systemd: `journalctl -u lodestar | grep "setup token"`.
   - Terminal: the console.
3. Enter the token, read and accept the Legal Notice, then choose the admin password. Anyone who can read the log
   can claim a fresh instance, so do this before exposing the port.
4. The token is generated fresh at each start while no admin exists, and is
   not stored. Once the admin password is set, no token is printed or needed.

In v0.5.1, do not assume setup keeps the crawler offline. Sampling waits six hours after each start, and the index grows over days.
Search remains available. The development behavior that waits for setup before starting the crawler is not in this release.

## 4. Networking

### 4.1 Ports
| Port | Use |
|---|---|
| `8090/tcp` | Web UI, API, `/metrics`, `/healthz`. Keep it on your LAN. |
| `6881/udp` | DHT. Forward it on your router for the best reach. |
| `6881/tcp` | Reserved for incoming peer connections **(planned)**. Fetches are outbound only today, so there's nothing to forward yet. |
| `8091/tcp` | Cluster port for workers. Closed unless `[cluster] enabled = true`. Keep it on your LAN. |

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
- **Add Lodestar's post-rules to gluetun.** gluetun's firewall (its kill
  switch) blocks new connections while the VPN is down, but as of v3.41.3 it
  lets packets of connections opened earlier leave through the normal
  network, with your own address. Download the file, pinned to the commit it
  was checked at, and check it (gluetun runs its lines as root):
  `https://raw.githubusercontent.com/dvortsis/lodestar-v2/dbad7937994c786675a65ee3b69e5058fd99e56e/packaging/companions/gluetun-post-rules.txt`,
  SHA-256 `ca05435e6f16ee2f44e35e79613a6a35d18f4019da1989cf11119deb348d746a`.
  Mount it read-only at `/iptables/post-rules.txt` in gluetun and set
  `VPN_INTERFACE=wg0` (gluetun's default is `tun0`, which the rules don't
  name; checked with WireGuard). Confirm with
  `docker exec gluetun iptables -S OUTPUT | head -4`: the three lines after the
  policy name `wg0`. Gluetun's "enabled successfully" log line alone proves
  nothing, because a missing file is skipped. Then a connection that used
  the tunnel never leaves another way. Every rule only marks or drops traffic.
  In compose, a `configs:` entry with the file's text copies it in, with no
  host file (`infra/fleet/steps/vm/compose.yaml`). If gluetun then stops with
  `ip6tables is required`, the kernel has no IPv6 firewall: remove the
  `ip6tables` lines. Restart Lodestar after gluetun.

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
- **One folder.** Everything Lodestar saves lives under `/data`. Map that
  one folder to the pool path above; there are no separate hot and cold
  mappings. To keep a backup on the array, run `lodestar backup` (it writes
  into the data folder), then copy that folder to a private share with
  `cp -a`. A backup holds `seal.key` and `auth.json`, so keep it private. See
  [Chapter 3, §3.7](user/03-keep-lodestar-safe.md#37-back-up-and-restore). `--to` only works for a
  path the container can see, which needs its own path mapping.

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
| `import-bitmagnet.cursor` | Resume point of the old `import-bitmagnet` command; the next import from the same database picks it up once and renames it `….bound-to-job-<N>` |

Swarm-statistics hot columns are plain in-memory atomics. A clean shutdown
snapshots them to `hot/*.col`; after a crash they're rebuilt from the
database at the next start (§9). Everything stays inside the one data folder;
the `hot/` folder is a subfolder of it, not a separate place.

## 7. Upgrades and Backups

- **Upgrading by hand:** stop Lodestar, replace the binary (or pull the new
  image tag), start it. Before upgrading, run `lodestar backup`. On Linux,
  Lodestar can also install updates itself. That is off until you turn it on in
  the app's update settings, and **Fixes only** is the recommended choice. A
  self-installed update that fails its health check rolls back on its own.
  **Windows** **(newer than v0.5.1)**: Lodestar checks and tells you. It installs only when you press
  **Install now** (MSI or portable); updates that install by themselves, and the
  app's **Roll back** button, are **(planned)**. A failed update (MSI or portable) rolls
  back by itself. On v0.5.1, Windows has no update engine: install the new MSI or ZIP by hand. Docker and Unraid don't self-install: you pull the new image
  tag.
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
- **`lodestar backup`** makes a consistent copy of the store, `config.toml`,
  `seal.key` and `auth.json` in a new `backups/lodestar-backup-<time>` folder
  inside the data folder (or `--to <folder>`). It's safe while Lodestar runs.
  The search index isn't copied: Lodestar rebuilds it from the store.
- **Update checks** are off until you turn them on (see above). When on, Lodestar checks the project's signed release page every 6 hours, and when you ask (on Windows, **(newer than v0.5.1)**). Lodestar sends no telemetry.

### Importing from Magnetico or Bitmagnet

**Development builds only:** these job commands and Magnetico imports are unavailable in v0.5.1. For the released Bitmagnet importer, run `lodestar import-bitmagnet --help` in a terminal beside the binary.

An import copies the torrents another crawler already found into Lodestar, so a new install doesn't start empty. Lodestar only reads the other database; it never changes it. Each import is a **job** with a number, which you can pause, resume, cancel or take back.

**Before you start:**
1. **Back up first, while Lodestar still runs:** `lodestar backup`. On Unraid, open the terminal and run `docker exec Lodestar /lodestar backup`. A backup is the safe undo; you can also take an import back (below).
2. **Stop the other crawler.** A database that's still being written is refused. For Magnetico, a copy works only if you copy its `-wal` file with it and checkpoint the copy first; if it's still refused after magneticod has stopped, see [development import troubleshooting](#development-import-troubleshooting).
3. **Stop Lodestar.** Imports run while the server is stopped. If it's running, `lodestar import start` refuses and changes nothing.

**On Unraid (or any Docker install),** a stopped container can't run commands. Instead, start a one-off container from the same image, with the same data folder, and with the folder that holds the Magnetico database:
```bash
docker run --rm -it -e PUID=99 -e PGID=100 \
  -v /mnt/cache/appdata/lodestar:/data \
  -v /mnt/user/appdata/magnetico:/import \
  ghcr.io/dvortsis/lodestar-v2:latest \
  import inspect --source magnetico --path /import/database.sqlite3
```
- `/mnt/cache/appdata/lodestar` is your Lodestar **Data** path, and `/mnt/user/appdata/magnetico` the folder with magneticod's `database.sqlite3`. Change both to yours.
- **Keep the database folder writable** (no `:ro`). SQLite needs to create two small files beside it (see Magnetico, below); a read-only folder is refused with `folder_not_writable`.
- Everything after the image name is the `lodestar` command. Run each command below the same way, replacing `import inspect …`.
- `--rm` removes the one-off container when it finishes. Your data stays in the data folder.

Outside Docker, run `lodestar import …` directly.

**Check the source first.** `inspect` only reads. It's safe while Lodestar runs:
```bash
lodestar import inspect --source magnetico --path /import/database.sqlite3
lodestar import inspect --source bitmagnet --host <host> --password-file <file>
```
It prints what the source is, how many torrents it holds, and how much space the import needs. If the data folder doesn't have room, it says so and the import is refused. Free space is checked on Linux and Unraid only, not on Windows.

**Try a pilot, then import the rest.** `--limit` pauses after about that many torrents:
```bash
lodestar import start --source magnetico --path /import/database.sqlite3 --limit 10000
lodestar import status
lodestar import resume 1 --source magnetico --path /import/database.sqlite3
```
- **Ctrl-C pauses the job at the last saved batch.** A second Ctrl-C quits at once. Either way, `resume` carries on from the last saved batch, with the same source flags. Nothing is imported twice.
- **`resume` checks the source is the same database** the job started on. If it changed, the job is blocked and says why. Start a new job for the new database.
- **Start Lodestar again afterwards.** It indexes the imported torrents and re-fetches incomplete file lists.
- **Every imported torrent goes through your content filters,** as found torrents do. A torrent the filters removed earlier is never brought back.
- Add `--json` to `inspect`, `start`, `resume`, `status` and `remove` for output a script can read.

**Magnetico:**
- **Two small files appear beside the database:** `database.sqlite3-wal` and `database.sqlite3-shm`. SQLite creates them whenever anything opens the database, even only to read it. They're harmless, and magneticod uses them too. **Leave them.** If magneticod stopped without shutting down cleanly, the `-wal` file holds torrents that aren't in the database file yet, and deleting it loses them.
- **A database on a network share is refused unless you allow that share:** `--allow-share \\nas\share`. Opening a share can send your Windows account's credentials to that server, so allow only a server you trust.
- **On Windows, use the database's drive letter** (`D:\magnetico\database.sqlite3`), not a volume path (`\\?\Volume{…}`), which is refused.
- **magneticod's oldest layouts (v0 and v1) are refused.** Run a current magneticod on the database once, which upgrades it, then import.

**Bitmagnet:**
- **Connection:** `--host`, `--port` (5432), `--dbname` (`bitmagnet`) and `--user` (`bitmagnet_reader`, the read-only role from `docs/operations/imports/bitmagnet-restore.md`).
- **Password:** from `--password-file` (keep it `chmod 600`) or the `LODESTAR_IMPORT_PG_PASSWORD` variable. There's no `--password`, because other users on the machine can see a command line.
- **TLS** is always used and verified, against the system's certificates or the ones in `--ca-file`. For a database on the same machine (a loopback address, or a socket folder such as `--host /var/run/postgresql`), `--allow-plaintext-loopback` skips it.
- **A database that's still being written is refused.** The check watches the whole PostgreSQL server, so run `VACUUM ANALYZE` on a restored copy before importing, as the restore guide does, and don't import from a server other apps write to.
- **In a one-off container, `--host localhost` is that container,** not your server. If Bitmagnet's PostgreSQL port is published on the same server, add `--network host` to `docker run` and use `--host 127.0.0.1 --allow-plaintext-loopback`. For a database on another machine, use its address; TLS is then required.
- **Put the password file in a mounted folder,** for example `-v /mnt/user/appdata/lodestar-import:/secrets:ro` with `--password-file /secrets/pg-password`.
- **`lodestar import-bitmagnet`** still works, as the old name for `lodestar import start --source bitmagnet`. Run again, it carries on with its unfinished job. If the old command left a cursor file, the new job carries on from it once.
  - **Torrents the old command imported before this version can't be taken back** with `lodestar import remove`, because it didn't record which import stored them. A backup from before that import is their only undo.

**Taking an import back.** Cancel the job, then remove what it imported:
```bash
lodestar import cancel 1
lodestar import remove 1
```
- `remove` asks first; `--yes` skips the question.
- **It deletes only what that job alone added.** It keeps a torrent another import also brought in, one Lodestar's crawler has found since, and one you've favourited, labelled, noted or put in a collection.
- **Filter decisions stay.** A torrent the filters removed during the import stays removed.
- **A job that can still read its source can't be removed:** one that's paused, blocked or running before it reached the end of the source. Cancel it first, so it can't later resume past what was removed. A job that read its whole source (it says `indexing`) can be removed as it is.
- **Start Lodestar afterwards.** The removed torrents leave search when it starts.

### Development import troubleshooting
<!-- index: Magnetico; import; source_not_quiescent; -wal file; WAL checkpoint -->

`lodestar import` refuses a Magnetico database with `source_not_quiescent` while magneticod may still be writing to it. The sign is a `database.sqlite3-wal` file that isn't empty, or a `database.sqlite3-journal` file, beside the database. The refusal changes nothing, and it stays even after magneticod stops if those files still hold changes.

The `-wal` file holds changes [SQLite](user/glossary.md#sqlite) hasn't written into the database file yet. magneticod writes them in, and removes or empties the file, when it stops cleanly. When that fails, for example because the disk is full, the `-wal` file keeps torrents that aren't in the database file.

To fix it:

1. If magneticod is running, stop it. Run the import again, as [the import guide](#importing-from-magnetico-or-bitmagnet) shows.
   **Result:** The import starts. If it's still refused, go on to step 2.
2. Free some disk space on the drive that holds the database. Writing the changes in needs room.
3. Start magneticod, wait until it's running, then stop it.
   **Result:** magneticod writes the changes into the database, and the `-wal` file is gone or empty. Run the import again.

If you can't run magneticod again, write the changes in on a copy with the `sqlite3` tool instead. This is for a leftover `-wal` file only. A `-journal` file means a change was cut off part-way; steps 1 to 3 undo it when magneticod opens the database again.

1. Copy `database.sqlite3` and `database.sqlite3-wal` together to a folder with free space. The database file alone is missing the newest torrents and can be damaged.
2. Open a terminal in that folder (on Unraid, the terminal button at the top of the web page, then `cd` to the folder) and paste:
   ```bash
   sqlite3 database.sqlite3 'PRAGMA wal_checkpoint(TRUNCATE);'
   ```
   **Result:** It prints `0|0|0`, and the `-wal` file is gone or empty. A first number of `1` means another program still has the copy open. If the terminal says `sqlite3: command not found`, copy the folder to a computer that has the `sqlite3` tool from [sqlite.org](https://sqlite.org/download.html), and run the command there.
3. Import the copy: point `--path` at it, and in Docker mount its folder instead ([the import guide](#importing-from-magnetico-or-bitmagnet)).

> [!WARNING]
> Never delete the `-wal` file: that loses the torrents in it. Write its changes in instead, with magneticod or `sqlite3` as above. A copy with its leftover `-wal` file beside it is refused like the original. Run step 2 on the copy before you import it.

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

**The web UI doesn't load.** Work through [I can't reach the UI](#i-cant-reach-the-ui)
below. Behind a TLS reverse proxy, set `LODESTAR_SECURE_COOKIES=true` (or
`http.secure_cookies = true` in `config.toml`).

**Slow disk, or SQLite "database is locked".** Check the data path. If it is
under `/mnt/user` on Unraid, move it to a pool path (§5). Lodestar warns at
start when it finds data on a FUSE or network filesystem; `lodestar stats`
shows it too.

**The router or network gets slow.** See §4.5; lower `LODESTAR_INTENSITY`.

**"Too many open files".** Under systemd raise `LimitNOFILE`; in Docker raise
`--ulimit nofile`. About 1000 sockets are used at `aggressive`.

**Lost the setup token.** While no admin exists, every start prints a new
token, so restart the service and read the log again.

### I can't reach the UI

Try these in order. Each is safe, and nothing changes until a step says so.

**Is Lodestar running?**
1. Run `lodestar healthcheck` (Docker: `docker exec <container name> /lodestar healthcheck`).
   **Result:** it exits 0 when the instance answers. Otherwise it exits 1 and prints `unhealthy:` and a reason, such as `unhealthy: connecting to 127.0.0.1:8090: Connection refused (os error 111)`. It probes the address Lodestar is set to use, so a wrong saved address shows up here.
2. If it fails, read the log (`docker logs <container name>`, `journalctl -u lodestar`, or the Windows log in §2).
   **Result:** the log says why it stopped, for example an address it can't bind.
3. If it answers, check the address you open. `LODESTAR_HTTP_BIND` (default `0.0.0.0:8090`; `0.0.0.0` means every network interface on the machine) is the address *inside* the container or machine. In Docker you open the host port you mapped to it. Keep the web port on your home network and never forward it on your router. On Windows, the Start-menu shortcut always opens `http://localhost:8090/`, so it won't follow a changed port. With host networking or a native Linux install, binding to your LAN address is narrower than `0.0.0.0`. In Docker or Unraid bridge mode (the default) leave it at `0.0.0.0`: the port mapping decides who can reach it. On Windows, binding off localhost also breaks the Start-menu shortcut, so keep the MSI's `WEB_FIREWALL` option off unless other PCs need the UI.

**The web address is wrong, so Lodestar won't start.**

The `lodestar settings` steps need a version newer than v0.5.1 **(newer than v0.5.1)**. On v0.5.1 (and `:latest` before the next release), `LODESTAR_HTTP_BIND` always works; `bind` under `[http]` in `config.toml` works only when no address is saved in the app.

If `serve` can't bind the address, it fails at start. There is no automatic revert for an address set from the command line, and the app has no screen for changing the address **(planned)**. Reset it:

1. Stop Lodestar, and run the commands as §10 describes for your install. In **Docker and Unraid** the image itself sets `LODESTAR_HTTP_BIND=0.0.0.0:8090`, so the address comes from the environment and `settings` can't change it: if you added your own `LODESTAR_HTTP_BIND`, correct or remove that variable, or change the port mapping. Otherwise continue.
2. Run `lodestar settings get network.http_bind`.
   **Result:** it shows the value and its source: `default`, `config.toml`, `saved` or the environment.
3. Act on the source.
   - **`saved`:** run `lodestar settings reset network.http_bind`.
     **Result:** the saved value is removed and `config.toml`'s value or the default (`0.0.0.0:8090`) applies again.
   - **`config.toml`:** edit or remove `bind` under `[http]` in that file. A saved value beats `config.toml`, so this only helps when the source says `config.toml`.
   - **environment:** correct or remove the variable. A variable always wins over a saved value.
4. Start Lodestar and open the web UI.
   **Result:** the sign-in page loads.

**I changed several settings and now nothing works.** This needs `lodestar settings` **(newer than v0.5.1)**.

`reset --all` also removes the passwords, the Torznab API key and the notification addresses and tokens saved in the app. Lodestar's own API keys aren't touched. Enter them again afterwards. To undo it, restore the backup from step 1 ([Chapter 3, §3.7](user/03-keep-lodestar-safe.md#37-back-up-and-restore); outside Unraid, the backup folder's `README.txt` repeats the steps).

1. Run `lodestar backup`. It works with the server running or stopped. On Docker, while the container runs, use `docker exec <container name> /lodestar backup`; once it's stopped, use the one-off container from §7 without the `/import` mapping, with `backup` after the image name.
   **Result:** a new `lodestar-backup-<time>` folder holds the store, `config.toml` (if you have one), `seal.key` and `auth.json`.
2. Stop Lodestar.
3. Run `lodestar settings reset --all`.
   **Result:** every value saved in the app is removed, and settings return to `config.toml` and the defaults. `config.toml`, the `LODESTAR_*` variables, your torrents, filters and admin password aren't touched.
4. Start Lodestar and open the web UI.

`reset --all` also clears a web-address change still waiting to be confirmed.

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

## 10. The Command Line

`lodestar` is one program. Run `lodestar --help`, or `lodestar <command> --help`, for the full list of options.

**Exit status:** 0 done, 1 failed, 2 bad arguments, 3 refused for safety (nothing was changed).

**Where to run it.**
- Linux with systemd: the service runs as its own account with data in `/var/lib/lodestar`. Don't run store commands (`settings`, `rollback`, `vacuum`) as root: systemd makes `/var/lib/lodestar` a link, and Lodestar refuses to drop privileges on a linked data folder, so the command stops with an error. To recover a wrong address, edit `/var/lib/lodestar/config.toml` only if no address is saved; a saved address beats it, and only an `Environment=LODESTAR_HTTP_BIND=` line in the unit overrides that. A tested way to run `lodestar settings` under the service account is **(planned)**.
- Windows: see §2.
- Docker (server running): `docker exec lodestar /lodestar <command>`. On Unraid the container is named `Lodestar`.
- Docker, "server stopped" commands: a stopped container can't run commands. Start a one-off container from the same image with the same `/data` folder, as shown in §7.
- `--config <path>` on any command picks a `config.toml`. Without it, Lodestar uses `$LODESTAR_DATA_DIR/config.toml`.

"Server stopped" below means the command refuses and changes nothing while Lodestar runs. That protects your data, so stop Lodestar rather than looking for a way round it.

| Command | What it does | Safe use |
|---|---|---|
| `serve` | Runs the crawler, search, web UI, status page and metrics. It's the default when you give no command. `--fixture [<dir>]` serves built-in test data offline instead. | Use `--fixture` to try the UI without crawling. |
| `healthcheck` | Exits 0 if the local instance answers `/healthz`, 1 with a reason otherwise. | Safe any time. |
| `stats` | Counts, sizes and the search index's state. `--json` prints JSON. | Safe while the server runs. |
| `backup` | Copies the store, `config.toml`, `seal.key` and `auth.json` into a new `lodestar-backup-<time>` folder in `<data>/backups`, or in `--to <folder>`. | Safe while the server runs. Copy the folder off the machine and keep it private: it holds `seal.key` and `auth.json`. |
| `rollback` | With no argument, lists the snapshots taken before each store upgrade. `rollback <name or vN>` restores one. `--update` rolls a self-installed update back to the version before it (Linux only today; on Windows a failed update rolls back by itself, or you reinstall the previous MSI). `--yes` skips the question. | Server stopped. List first, then restore; the replaced store is kept (§7). |
| `vacuum` | Compacts the store and prints its size before and after. | Server stopped. Back up first. |
| `reindex` | Rebuilds the search index from the store at the next start. | Server stopped. Search is slow while it rebuilds. |
| `rollup --repair` | Recounts the Insights totals from the stored torrents. `--since <N>d` sets how far back (default 30 days). | Server stopped. It only raises totals. |
| `lists` | Shows the blocklist catalog. | Safe any time. |
| `import …` **(newer than v0.5.1)** | Imports a Magnetico or Bitmagnet database: `inspect`, `start`, `status`, `pause`, `resume`, `cancel`, `remove`. | Back up first. See §7. |
| `import-bitmagnet` | The old name for `import start --source bitmagnet`. | Prefer `import`. |
| `companion …` **(newer than v0.5.1)** | Lists, installs or removes a companion, or prints a compose service for it (`list`, `install`, `remove`, `compose`). Without `--sha256`, `install` only shows what it would install; without `--yes`, `remove` only shows what it would delete. | `install` and `remove` need the server stopped. Install only what you recognise, and confirm with the SHA-256 `install` showed. |
| `worker` **(newer than v0.5.1)** | Runs this install as a worker for a main Lodestar: `--join <https://host:port>` with `--token-file <file>` (or `-` for stdin) joins once; afterwards it runs from the saved credentials. `--rotate-key` re-seals them under a new key. | Give the worker a data folder of its own. The token is never taken from the command line, because other users can see command lines. |
| `settings …` **(newer than v0.5.1)** | Reads and changes the settings saved in the app, with the server stopped. See below. | Use it to recover; change everyday settings in the app. |

### Settings from the command line **(newer than v0.5.1)**

Use these when the app can't be reached. They check each value exactly as the app does.

- `lodestar settings get [<section>[.<field>]]` shows a value and where it comes from: the default, `config.toml`, a saved value or an environment variable. With a section, or nothing, it lists every field. A secret shows only as `(set)` or `(not set)`.
- `lodestar settings set <section>.<field> <value>` saves a value (server stopped). Give it as JSON (`true`, `30`, `"text"`); anything that isn't JSON is taken as text, so `0.0.0.0:8080` needs no quotes.
- `lodestar settings reset <section>.<field>` removes a saved value, so `config.toml`'s value or the default applies again.
- `lodestar settings reset --all` removes every saved value. `config.toml`, the `LODESTAR_*` variables and your filters, floor, packs and subscriptions stay as they are.

A `LODESTAR_*` variable always wins over a saved value, so `set` and `reset` refuse a field a variable sets (exit 2).

**What `set` refuses (exit 3, nothing changed), and why.**
- **Secrets**, such as passwords, API keys and notification URLs and tokens. They're refused before the value is read, so a secret is never echoed or logged, and it never needs to be a recovery step. Set them in the app.
- **Privacy opt-ins**, such as detailed logging and query logging, whatever the value. `reset` turns each back off.
- **Security-tagged fields**, which in the app ask for your password again. None exist today. Any other danger tag a later version adds is refused too.
- **Turning on adult content.** The app asks for a confirmation the command can't give. Turning it off is allowed.
- Any other change the app refuses is refused with the app's own message (exit 2).

`set` and `reset` also refuse while an update rollback is in progress. After changing `network.http_bind`, a web-address change still waiting to be confirmed is cleared, because you are at the machine and have confirmed the address by running the command.

### The Windows update helper **(newer than v0.5.1)**

`lodestar update-helper` belongs to the MSI. Windows runs it, as SYSTEM, from a scheduled task called **Lodestar update helper**, which the installer adds and the uninstaller removes. You never run it yourself. When you press **Install now**, the helper installs the update; if it fails its health check, the helper puts the old version back by itself. To go back by hand, reinstall the previous MSI.
