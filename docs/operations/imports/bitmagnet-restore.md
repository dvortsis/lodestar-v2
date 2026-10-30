# Restoring a Bitmagnet database for import

This runbook turns a Bitmagnet 0.10.0 backup into a PostgreSQL 16 database that Lodestar's importer can read safely. It covers both inputs the owner holds:

- **A. the dump:** `backup.zip` holding `backup.sql`, a plain-SQL `pg_dump` (about 97 GiB unpacked, `SQL_ASCII`, column-named `INSERT`s);
- **B. the data directory:** a PostgreSQL 16 cluster folder (`postgres-data`, with `PG_VERSION` = `16`).

It ends with a read-only role for the importer and `bitmagnet_check` passing. It does not import anything: that's the later `import-bitmagnet` step (plan `docs/plan/2026-10-29-magnetico-bitmagnet-migration.md`, scope banner first).

Commands are given for **Unraid** (the web terminal, bash) and **Windows with Docker Desktop** (PowerShell). Where only one block is shown, it is the same on both.

## Rules this runbook keeps

1. **The originals are never touched.** They're made read-only, checksummed before and after, and only ever mounted read-only.
2. **PostgreSQL runs with no network.** The container gets `--network none` and listens on no TCP address. You reach it with `docker exec`, and the checker and importer reach it through a Unix socket in a shared Docker volume.
3. **No byte is converted.** The database is created `SQL_ASCII` and the dump is loaded with client encoding `SQL_ASCII`; PostgreSQL never converts to or from `SQL_ASCII`. `SQL_ASCII` means "encoding unknown", not "ASCII": the names are a mix of UTF-8, Latin-1, GBK and broken bytes, and the importer decodes them later, keeping the original bytes' hash.
4. **Passwords never go on a command line.** They live in files in a Docker volume, readable only by the container's `postgres` user.
5. **Check free space before every step that writes.**

## Terms

- **Container:** a program running isolated from the rest of the computer, from an *image* (here `postgres:16-alpine`).
- **Volume:** storage a container sees as a folder. A *bind mount* is a host folder; a *named volume* is one Docker manages.
- **Cluster / data directory:** the folder where PostgreSQL keeps all its databases.
- **WAL:** PostgreSQL's write-ahead log, extra files written alongside the data.
- **Unix socket:** a file that programs on the same machine connect through instead of a network port.

## 0. Before you start

- **Docker:** Unraid's built-in Docker, or Docker Desktop on Windows using the WSL 2 backend.
- **Docker Desktop's disk limit (Windows).** Everything in a named volume lives in Docker Desktop's virtual disk, on `C:` unless you moved it. Open **Settings → Resources → Advanced** and check that the disk image limit is at least the total from step 2, and that its drive has that much free. Raise it there if not.
- **The repository** cloned on the machine, for the helper files in `docs/operations/imports/bitmagnet-restore/` and to build the checker.
- **Bitmagnet 0.10.0's migrations,** for the dump (input A) only. On any machine with internet access: `git clone --depth 1 --branch v0.10.0 https://github.com/bitmagnet-io/bitmagnet`, then copy its `migrations` folder into the work folder (step 1).
- **Stop Bitmagnet** if it still runs anywhere against these files. Don't start its crawler at any point.

## 1. Choose the folders

The work folder holds the copies, the restored cluster and the logs. Put it on a disk with the space from step 2.

- **Unraid:** use a direct disk or pool path, such as `/mnt/disk1/lodestar-restore` or `/mnt/cache/lodestar-restore`, not `/mnt/user/…`: PostgreSQL is much slower through the user-share layer. The restored cluster is a plain folder there.
- **Windows:** use a folder on a roomy drive, such as `D:\lodestar-restore`, for the dump copy and logs. The restored cluster goes in a **named volume** (`bitmagnet-pgdata`), inside Docker Desktop's disk: PostgreSQL can't run on a Windows folder, which lacks the file permissions it needs.

Unraid:

```bash
O=/mnt/user/bitmagnet-originals          # where backup.zip and/or postgres-data are now
W=/mnt/disk1/lodestar-restore            # the work folder
REPO=/mnt/user/appdata/lodestar-v2       # your clone of the repository
mkdir -p "$W/dump" "$W/logs" "$W/pgdata" "$W/migrations"
DATA="$W/pgdata"
```

Windows (PowerShell):

```powershell
$O = "D:\bitmagnet-originals"            # where backup.zip and/or postgres-data are now
$W = "D:\lodestar-restore"               # the work folder
$REPO = "D:\src\lodestar-v2"             # your clone of the repository
New-Item -ItemType Directory -Force "$W\dump", "$W\logs", "$W\migrations" | Out-Null
$DATA = "bitmagnet-pgdata"
docker volume create bitmagnet-pgdata
```

Then, on both, create the socket and secrets volumes and the two passwords:

```bash
docker volume create bitmagnet-socket
docker volume create bitmagnet-secrets
docker run --rm --network none -v bitmagnet-secrets:/secrets -v "${REPO}/docs/operations/imports/bitmagnet-restore:/restore/sql:ro" postgres:16-alpine sh /restore/sql/make-secrets.sh
```

It prints `wrote /secrets/admin_pw` and `wrote /secrets/reader_pw`. Run again, it keeps them.

Use one fixed image for every step: `postgres:16-alpine`, version 16.10 or later (`docker run --rm postgres:16-alpine psql --version`). 16.10 is the first psql that understands the `\restrict` line new dumps start with. Bitmagnet's own compose file uses `postgres:16-alpine` too, which matters for input B (step 5).

## 2. Size the disk

Work out the space before you copy anything, and keep a reserve so the volume never fills: a full disk in the middle of a restore can corrupt it.

| Item | Input A (dump) | Input B (data directory) |
|---|---|---|
| Copy of the input | `backup.sql`, 103,805,668,425 bytes ≈ 97 GiB | the size of `postgres-data` (measure it below) |
| Restored cluster | plan for 2 × the dump ≈ 195 GiB | none extra: the copy *is* the cluster |
| WAL and temporary files | 20 GiB | 10 GiB |
| Reserve, always free | 50 GiB | 50 GiB |
| **Total** | **≈ 362 GiB** | **copy + 60 GiB** |

The "2 × the dump" is an estimate: table data plus Bitmagnet's indexes. Watch the real growth during the restore (step 4) and stop if the reserve is in danger. After a successful check (step 7) you can delete `backup.sql` and get 97 GiB back.

Measure the inputs and the free space.

Unraid:

```bash
ls -l "$O/backup.zip"; unzip -l "$O/backup.zip"   # one member, backup.sql
du -sh "$O/postgres-data"
df -h "$W"
```

Windows:

```powershell
Get-Item "$O\backup.zip" | Select-Object Length
(Get-ChildItem -Recurse "$O\postgres-data" | Measure-Object Length -Sum).Sum / 1GB
Get-PSDrive D, C                                   # D: work folder, C: Docker Desktop's disk
docker run --rm --network none -v bitmagnet-pgdata:/v postgres:16-alpine df -h /v
```

On Windows the last line shows the free space inside Docker Desktop's disk, which is what the restored cluster uses.

**Check `df` again before steps 3, 4 and 5.** Each of them writes a lot.

## 3. Protect the originals and make the copy

Make the originals read-only and record their checksums. Hashing 22 GiB takes a few minutes.

Unraid:

```bash
chmod -R a-w "$O"
sha256sum "$O/backup.zip" > "$W/originals.sha256"
```

Windows:

```powershell
attrib +R "$O\*" /S /D
Get-FileHash "$O\backup.zip" -Algorithm SHA256 | Format-List | Out-File "$W\originals.sha256"
```

For the data directory, record its file list and sizes instead of hashing it all: `find "$O/postgres-data" -type f -printf '%s %p\n' | sort -k2 > "$W/originals.list"` (Unraid) or `Get-ChildItem -Recurse "$O\postgres-data" -File | Select-Object Length, FullName | Out-File "$W\originals.list"` (Windows).

**Input A, the dump.** Unpack `backup.sql` from the read-only original into the work folder. This copy is what the restore reads.

```bash
df -h "$W"                                          # ≥ 97 GiB plus the reserve
unzip "$O/backup.zip" backup.sql -d "$W/dump"       # Unraid
```

If Unraid answers `unzip: command not found`, unpack it in a throwaway container instead. It needs the internet to install `unzip`, and sees the original read-only: `docker run --rm -v "$O:/o:ro" -v "$W/dump:/d" alpine:3 sh -c "apk add --no-cache unzip && unzip /o/backup.zip backup.sql -d /d"`.

```powershell
tar -xf "$O\backup.zip" -C "$W\dump" backup.sql     # Windows (tar.exe reads ZIP)
```

Then look at what the dump is, without running it:

```bash
head -c 5 "$W/dump/backup.sql"; echo                                    # "PGDMP" = custom format, else plain SQL
grep -a -m1 "client_encoding" "$W/dump/backup.sql"                      # SET client_encoding = 'SQL_ASCII';
head -c 67108864 "$W/dump/backup.sql" | grep -a -c "^CREATE TABLE"           # 0 = data-only
grep -a -E '^\\[a-zA-Z!]' "$W/dump/backup.sql" | cut -d' ' -f1 | sort | uniq -c   # psql commands in it
```

On Windows, run the same four lines in a throwaway container: `docker run --rm -it --network none -v "${W}\dump:/dump:ro" postgres:16-alpine sh`, then use `/dump/backup.sql` in place of `"$W/dump/backup.sql"`.

What to expect, and what to do otherwise:

- **Plain SQL** (no `PGDMP`). A custom-format dump would need `pg_restore` instead of `restore.sh`; stop and ask.
- **`client_encoding = 'SQL_ASCII'`.** Anything else: stop and ask, because the steps below assume it.
- **Data-only (0 `CREATE TABLE`).** Bitmagnet's backup guide makes data-only dumps, so step 4 creates the schema from the migrations first. If the dump does contain `CREATE TABLE`, skip `apply-migrations.sh`.
- **The only psql commands are `\restrict` and `\unrestrict`.** Any other line starting with a backslash, `\!` above all, could run commands: stop and look at it before going on.

**Input B, the data directory.** Copy it while nothing is running on it.

1. Make sure no PostgreSQL uses it: no Bitmagnet container running (`docker ps`), and on Windows no Docker Desktop project mounting it.
2. Check the version: `cat "$O/postgres-data/PG_VERSION"` (Unraid) or `Get-Content "$O\postgres-data\PG_VERSION"` (Windows) must print `16`. Anything else needs that version's image; stop and ask.
3. Copy it.

Unraid:

```bash
df -h "$W"
cp -a "$O/postgres-data/." "$DATA/"
rm -f "$DATA/postmaster.pid"                        # the copy's stale lock file, never the original's
chmod -R u+w "$DATA"; chown -R 70:70 "$DATA"; chmod 700 "$DATA"
```

Windows (into the named volume; the original is mounted read-only):

```powershell
docker run --rm --network none -v "${O}\postgres-data:/from:ro" -v bitmagnet-pgdata:/to postgres:16-alpine sh -c "cp -a /from/. /to/ && rm -f /to/postmaster.pid && chown -R 70:70 /to && chmod -R u+w /to && chmod 700 /to"
```

`postmaster.pid` is a lock file a running server leaves. If the original has one, Bitmagnet was probably not shut down cleanly; PostgreSQL will replay its WAL on first start (step 5), which is normal.

## 4. Input A: start PostgreSQL and restore the dump

Start the isolated server. Unraid:

```bash
chown 70:70 "$W/logs"                               # the container's postgres user writes the log
docker run -d --name bitmagnet-restore --network none --shm-size 1g \
  -e POSTGRES_PASSWORD_FILE=/run/secrets/admin_pw \
  -e POSTGRES_INITDB_ARGS="--encoding=SQL_ASCII --locale=C --auth-local=peer --auth-host=reject" \
  -v "$DATA:/var/lib/postgresql/data" \
  -v bitmagnet-socket:/var/run/postgresql \
  -v bitmagnet-secrets:/run/secrets:ro \
  -v "$W/dump:/restore/dump:ro" \
  -v "$W/migrations:/restore/migrations:ro" \
  -v "$W/logs:/restore/logs" \
  -v "$REPO/docs/operations/imports/bitmagnet-restore:/restore/sql:ro" \
  postgres:16-alpine -c listen_addresses=
```

Windows: the same command with a backtick `` ` `` instead of each `\` at the line ends, `${W}\dump` style paths, and `-v "${DATA}:/var/lib/postgresql/data"`.

- `--network none` and `listen_addresses=` (empty): no network, no TCP port.
- `--auth-local=peer --auth-host=reject`: until step 6, only `docker exec -u postgres` gets in.
- `POSTGRES_INITDB_ARGS`: a new cluster is created `SQL_ASCII` with the byte-order `C` locale.

Wait until `docker logs bitmagnet-restore` ends with `database system is ready to accept connections`. Then create the database, switch to the fast restore settings, create the schema and start the restore:

```bash
docker exec -u postgres bitmagnet-restore psql -X -v ON_ERROR_STOP=1 -c "CREATE DATABASE bitmagnet ENCODING 'SQL_ASCII' LC_COLLATE 'C' LC_CTYPE 'C' TEMPLATE template0"
docker exec -u postgres bitmagnet-restore psql -X -v ON_ERROR_STOP=1 -f /restore/sql/restore-settings.sql
docker exec -u postgres bitmagnet-restore sh /restore/sql/apply-migrations.sh
docker exec -d -u postgres bitmagnet-restore sh /restore/sql/restore.sh
```

- `TEMPLATE template0` and `ENCODING 'SQL_ASCII'` are what keep the database's declared encoding. Never create it from `template1` or with another encoding.
- `restore-settings.sql` turns off crash safety (`fsync`) for speed. **If the host or container crashes during the restore, delete the cluster and start step 4 again;** it may be silently damaged.
- `restore.sh` refuses to run unless the database is `SQL_ASCII`, and loads the dump with client encoding `SQL_ASCII` and `ON_ERROR_STOP`, so the first error ends it. `docker exec -d` keeps it running when you close the terminal.

**Watch it.** The restore takes hours. Check progress and space now and then:

```bash
docker exec -u postgres bitmagnet-restore psql -X -d bitmagnet -c "SELECT relname, n_live_tup FROM pg_stat_user_tables ORDER BY n_live_tup DESC LIMIT 5"
docker exec -u postgres bitmagnet-restore psql -X -c "SELECT pg_size_pretty(pg_database_size('bitmagnet'))"
df -h "$W"                                          # Windows: the df line from step 2
```

It's done when `logs/restore.log` ends with `restore finished …, psql exit 0`. Any other exit code means it stopped at an error, shown just above that line. Fix the cause, then drop the database (`DROP DATABASE bitmagnet` as above) and start this step's commands again from `CREATE DATABASE`.

**When it finished,** go back to safe settings, update the planner's statistics (the checker's estimates use them), and restart:

```bash
docker exec -u postgres bitmagnet-restore psql -X -v ON_ERROR_STOP=1 -f /restore/sql/normal-settings.sql
docker exec -u postgres bitmagnet-restore psql -X -d bitmagnet -c "ANALYZE"
docker restart bitmagnet-restore
```

Continue at step 6.

## 5. Input B: start PostgreSQL on the copied data directory

Start the same container as in step 4, without `POSTGRES_INITDB_ARGS` (the cluster exists, so nothing is initialised) and without the dump and migrations mounts:

```bash
docker run -d --name bitmagnet-restore --network none --shm-size 1g \
  -v "$DATA:/var/lib/postgresql/data" \
  -v bitmagnet-socket:/var/run/postgresql \
  -v bitmagnet-secrets:/run/secrets:ro \
  -v "$REPO/docs/operations/imports/bitmagnet-restore:/restore/sql:ro" \
  postgres:16-alpine -c listen_addresses=
```

- Use **`postgres:16-alpine`**, as Bitmagnet did. A different C library (Debian's glibc instead of Alpine's musl) can sort text differently and silently break the text indexes.
- `docker logs bitmagnet-restore` may show `automatic recovery in progress` before `ready to accept connections`. That's the WAL replay, and it happens on the copy only.

Lock the logins down to the socket at once, because the copied `pg_hba.conf` is Bitmagnet's own:

```bash
docker exec -u postgres bitmagnet-restore cp /restore/sql/pg_hba.conf /var/lib/postgresql/data/pg_hba.conf
docker exec -u postgres bitmagnet-restore psql -X -c "SELECT pg_reload_conf()"
```

If the second command fails with `role "postgres" does not exist`, Bitmagnet used another admin name: `docker exec -u postgres bitmagnet-restore sh -c 'grep -v "^#" "$PGDATA/pg_hba.conf"'` shows the file, and the name is in Bitmagnet's compose file (`POSTGRES_USER`). Put it in place of `postgres` in the `local all postgres peer` line (edit a copy of the helper folder) and use `-U <name>` below.

Note the database's name and encoding, because the checker compares the encoding:

```bash
docker exec -u postgres bitmagnet-restore psql -X -c "SELECT datname, pg_encoding_to_char(encoding), datcollate FROM pg_database"
```

The steps below assume the database is called `bitmagnet`. If its encoding isn't `SQL_ASCII`, pass that name to the checker with `--encoding` (step 7); that's the data directory's own declared encoding, not a conversion.

## 6. Create the importer's read-only role

The role can log in to `bitmagnet` over the socket only, and read only `torrents`, `torrent_files` and `torrent_pieces`. Every transaction it runs is read-only. Its password is the `reader_pw` file from step 1.

```bash
docker exec -u postgres bitmagnet-restore cp /restore/sql/pg_hba.conf /var/lib/postgresql/data/pg_hba.conf
docker exec -u postgres bitmagnet-restore psql -X -c "SELECT pg_reload_conf()"
docker exec -u postgres bitmagnet-restore psql -X -d bitmagnet -f /restore/sql/reader-role.sql
```

It ends with `bitmagnet_reader is ready`. If it prints `STOP: … reader_pw …`, the secret is missing or unreadable: rerun `make-secrets.sh` (step 1) and this step. It never creates the role without a password.

The importer gets the password the same way the checker does: the `bitmagnet-secrets` volume mounted read-only and `--password-file /run/secrets/reader_pw`, or the `PGPASSWORD` environment variable. Never pass it as an argument.

**If the importer can't use the socket** (it runs outside Docker, on Windows): use a loopback-only port instead. This gives the container a network, so do it only if you need it:

1. Recreate the container with `-p 127.0.0.1:55432:5432` in place of `--network none`, and `-c listen_addresses=*` in place of `-c listen_addresses=`. Use Docker 28 or later; older versions could expose loopback-published ports to the local network.
2. Add `host bitmagnet bitmagnet_reader samenet scram-sha-256` to a copy of `pg_hba.conf`, copy it in and reload as above.
3. Connect to `127.0.0.1` port `55432`. Nothing else should listen there.

## 7. Verify with the checker

`bitmagnet_check` checks, without changing anything:

- the server is PostgreSQL 16;
- the database declares `SQL_ASCII` (or `--encoding`);
- the tables and columns the importer reads exist with Bitmagnet 0.10.0's types; other tables and columns are listed, not failed;
- exact row counts of `torrents`, `torrent_files` and `torrent_pieces`, and the planner's estimate for every table;
- a sample of `torrents.name` and `torrent_files.path`: the SHA-256 of the stored bytes and how many are valid UTF-8;
- free space on the cluster's volume, against `--reserve`.

It prints JSON on stdout and a summary on stderr, and exits 0 when every check passed, 1 when one failed, 2 on bad usage or no connection.

**Build it once,** in a Rust container (this one needs the internet, to fetch crates; the database stays offline):

```bash
docker run --rm -v "${REPO}:/src" -w /src -e CARGO_TARGET_DIR=/src/target/restore-check rust:1-bookworm cargo build --release --locked -p lodestar-sources-importers --bin bitmagnet_check
```

The binary lands in `target/restore-check/release/` in your clone.

**Run it.** It connects as the importer's role from step 6, so it also proves that role works. Unraid:

```bash
docker run --rm --network none \
  -v bitmagnet-socket:/var/run/postgresql:ro \
  -v bitmagnet-secrets:/run/secrets:ro \
  -v "$DATA:/pgdata:ro" \
  -v "$REPO/target/restore-check/release:/opt/check:ro" \
  rust:1-bookworm /opt/check/bitmagnet_check \
    --host /var/run/postgresql --dbname bitmagnet --user bitmagnet_reader \
    --password-file /run/secrets/reader_pw \
    --data-dir /pgdata --reserve 50GiB > "$W/logs/check.json"
```

Windows: the same, with backticks, `-v "${DATA}:/pgdata:ro"` and `-v "${REPO}\target\restore-check\release:/opt/check:ro"`, and `> "$W\logs\check.json"`.

On a database the size of the owner's, the exact counts take minutes. If a count stops at the statement timeout, raise it (`--statement-timeout 3600`) or skip exact counts (`--counts estimate`).

**Reading the result:**

- **`schema` fails:** the restore is incomplete or isn't Bitmagnet 0.10.0. The JSON names each missing column and wrong type.
- **`encoding` fails:** the database was created with the wrong encoding, so text may have been converted. Delete it and redo step 4.
- **`row_counts`:** compare the `torrents` count with what Bitmagnet showed (about 45 million for the dump).
- **`samples`:** `utf8_valid` below `rows` is expected for `SQL_ASCII` data. Keep the two `sha256` values: running the checker again with the same `--seed` on an unchanged database gives the same ones, so they show later that nothing changed the bytes.
- **`disk` fails:** free space before going on.

Keep `check.json` with the import records.

## 8. Teardown and cleanup

When the import is finished and checked:

```bash
docker stop -t 120 bitmagnet-restore && docker rm bitmagnet-restore
docker volume rm bitmagnet-socket bitmagnet-secrets
```

Then delete the work copies:

- **Unraid:** `rm -rf "$W/pgdata" "$W/dump"` (or all of `$W`).
- **Windows:** `docker volume rm bitmagnet-pgdata`, then delete `$W`. Docker Desktop doesn't give the freed space back to Windows by itself: **Troubleshoot → Clean / Purge data** does, but removes every container, image and volume.

Finally, confirm the originals didn't change:

```bash
sha256sum -c "$W/originals.sha256"                  # Unraid: backup.zip: OK
```

```powershell
Get-FileHash "$O\backup.zip" -Algorithm SHA256      # Windows: compare with originals.sha256
```

For the data directory, compare the file list as in step 3. Leave the originals read-only.

## Helper files

All in `docs/operations/imports/bitmagnet-restore/`, mounted read-only at `/restore/sql`:

| File | What it does |
|---|---|
| `make-secrets.sh` | writes the two random passwords into the secrets volume, readable only by the container's `postgres` user |
| `restore-settings.sql`, `normal-settings.sql` | the fast restore settings, and back to the defaults |
| `apply-migrations.sh` | creates Bitmagnet 0.10.0's schema from its own migrations, for a data-only dump |
| `restore.sh` | loads `backup.sql` with no encoding conversion, logging to `/restore/logs/restore.log` |
| `pg_hba.conf` | socket-only logins: the importer's role by password, the admin from inside the container |
| `reader-role.sql` | creates or updates the importer's read-only role |
