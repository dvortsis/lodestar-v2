# Run sheet: CR-AB-COMP, the pinned bitmagnet comparator

Checked by the runner, 2026-11-07T00:45Z. Rules: `docs/decisions/2026-11-02-CR-SPEC.md` (2.2, 5.4, 6.5, 6.6, 6.11). Tool: `docs/soak/tools/linux/collect.py` (CR-COL-3). All times are UTC. Write no IP addresses anywhere in your notes.
The owner confirmed CR-SPEC on 2026-11-04 (§2.34). Book the run by telling the runner the planned start time (UTC); the booking is what makes `ext:experiment-host` hold for this run.

## What this run is
bitmagnet crawls the DHT on your Unraid box for 25 hours: 60 min warm-up, then 24 h measured. The collector counts stored torrents once a minute. The result shows what "100 per minute" means for bitmagnet on your network. The DHT UDP port stays unforwarded, as for every run. There is no VPN.

## 1. Preconditions (tick every box before step 5)
- [ ] Every Lodestar container on the box is stopped and removed. The evidence folders are kept, not deleted.
- [ ] Nothing crawls on the box or from the home public IP for 12 h before bitmagnet starts (no Lodestar, no other crawler, no torrent client doing DHT).
- [ ] Parity check, mover and auto-updates are paused. Write down each one's next normal run time (UTC): parity ____ mover ____ auto-update ____
- [ ] Appdata Backup stays disabled (so the collector gets `backup=not_applicable`).
- [ ] No imports of any kind on the box.
- [ ] Router and network unchanged since the last run. Do not touch the router during the run.
- [ ] Port 3334 (bitmagnet's DHT port) is NOT forwarded. Do not publish it with `-p` either.
- [ ] The box clock is synchronised (NTP on). The offset must be within 1 s.
- [ ] `/mnt/user/appdata/bitmagnet-comp` does not exist yet, and `.../lodestar-soak/cr-ab-comp-1` does not exist yet. The evidence share has at least 50 GB free.
- [ ] Time now (UTC) ____. Planned start (UTC) ____, at least 12 h after the last crawling stopped.

## 2. Pinned images (checked 2026-11-07 against the registries)
| Image | Tag | Pin it as |
|---|---|---|
| bitmagnet | v0.10.1 (same digest as `latest` and `v0.10`) | `ghcr.io/bitmagnet-io/bitmagnet@sha256:b6373349a30193b436ce6b05e21f91aa4934074268105dded1f3b065b8c86f19` |
| Postgres | 16-alpine (what bitmagnet's compose example uses) | `postgres@sha256:aa6cb33cf4c19c765cefcf48d96176258800870a9a1e930ad126200f9d825139` |

Both are multi-arch indexes; Docker picks your CPU's build. Always run the `@sha256` form, never the tag alone. Before the run, confirm nothing moved: `docker pull <pin>` then `docker image inspect <pin>`.

## 3. Set-up
bitmagnet's own example runs `worker run --all`. For DHT crawling and classification only three workers are needed, so use `--keys`. Run `docker run --rm <bitmagnet pin> worker list` first to confirm those key names.

Settings, all by environment variable (the same rule as Lodestar: no config file):
- `POSTGRES_HOST=bitmagnet-postgres`, `POSTGRES_PASSWORD=<a password you choose>`. User defaults to `postgres`, database to `bitmagnet`, port to 5432. These match the collector's defaults (`--postgres-user postgres`, `--postgres-db bitmagnet`).
- `TMDB_ENABLED=false` (runner's call): Lodestar makes no outside metadata lookups by default, so bitmagnet doesn't either. The collector counts `torrents` rows; classification (keywords, file types) still runs.
- The DHT port is bitmagnet's default, 3334 (`DHT_SERVER_PORT`). Leave it. Do not publish it.
- `--restart no` on both containers. A restart ends the run.

```sh
docker network create bitmagnet-net
mkdir -p /mnt/user/appdata/bitmagnet-comp/postgres      # fresh, empty

docker run -d --name bitmagnet-postgres --restart no --network bitmagnet-net --shm-size 1g \
  -v /mnt/user/appdata/bitmagnet-comp/postgres:/var/lib/postgresql/data \
  -e POSTGRES_PASSWORD=<password> -e POSTGRES_DB=bitmagnet \
  postgres@sha256:aa6cb33cf4c19c765cefcf48d96176258800870a9a1e930ad126200f9d825139

docker run -d --name bitmagnet --restart no --network bitmagnet-net \
  -e POSTGRES_HOST=bitmagnet-postgres -e POSTGRES_PASSWORD=<password> -e TMDB_ENABLED=false \
  ghcr.io/bitmagnet-io/bitmagnet@sha256:b6373349a30193b436ce6b05e21f91aa4934074268105dded1f3b065b8c86f19 \
  worker run --keys=http_server --keys=queue_server --keys=dht_crawler
```
Do not paste these yet; step 5 gives the order. The names `bitmagnet` and `bitmagnet-postgres` are exact: the collector looks them up.

## 3a. Before the day (during the 12 h wait)
1. **The bitmagnet commit** for `--commit`: `docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' <bitmagnet pin>`. It prints a 40-character hex id; use it lowercase. If it prints nothing, tell the runner before the run.
2. **Confirm the worker keys and settings:** `docker run --rm <bitmagnet pin> worker list` shows `http_server`, `queue_server` and `dht_crawler`; `docker run --rm -e TMDB_ENABLED=false <bitmagnet pin> config show` shows TMDB off and the DHT port 3334. If either differs, tell the runner.
3. **A 15-minute rehearsal** (test data, never the run). Do step 5's order with the collector flags `--rehearsal --warmup-minutes 1 --baseline-hours 0.2 --root /evidence/cr-ab-comp-rehearsal`. It shows the `torrents` table appears and the first count reads 0 within 180 s, and that `--stop` stops bitmagnet cleanly. Then remove both containers and **delete** `/mnt/user/appdata/bitmagnet-comp/postgres`, so the real run starts from an empty store. The rehearsal is bitmagnet, not Lodestar, so it doesn't restart the 12 h clock; finish it at least 1 h before the planned start.
4. **Record the CPU model** in your own notes (`grep -m1 "model name" /proc/cpuinfo`).

## 4. The collector command
Put `collect.py` (from `docs/soak/tools/linux/`) at `/path/to/collect.py`. Fill in `<...>` first. The comparator needs no password file and no Lodestar URL. `--stop` makes the collector stop bitmagnet cleanly when the window ends.
```sh
docker run -d --name cr-ab-comp-collector --restart no --network host \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v /sys/fs/cgroup:/host/cgroup:ro -v /proc:/host/proc:ro \
  -v /mnt/user/appdata/lodestar-soak:/evidence \
  -v /path/to/collect.py:/collect.py:ro \
  python:3.12-slim python /collect.py --mode docker --container bitmagnet \
    --postgres-container bitmagnet-postgres \
    --cgroup-root /host/cgroup --proc-root /host/proc \
    --root /evidence/cr-ab-comp-1 --storage-path /evidence \
    --commit <bitmagnet source commit, from step 3a> \
    --spec-commit c011f8a679e80875e5369b56328bc1376fee2da7 \
    --run-kind comparator --stop \
    --checklist auto_update=excluded --next-run auto_update=<UTC time from step 1> \
    --checklist backup=not_applicable
```
The Docker socket is full control of the box. Use the pinned `python:3.12-slim` you already use for other runs.

## 5. The order (one terminal, no pauses)
1. Check every box in step 1. Note the time.
2. Start Postgres (step 3). Wait until `docker exec bitmagnet-postgres pg_isready` says it accepts connections.
3. Start bitmagnet (step 3). Its start time plus 60 min is the window start.
4. Start the collector (step 4) at once. Best: paste steps 2 to 4 as one script. The collector counts 5 s after it attaches. That first count must read 0 and must come within 180 s of bitmagnet starting. A first count above 0 voids the run.
5. Within a minute: `docker inspect -f '{{.State.StartedAt}}' bitmagnet` and write the time down. Check `docker logs bitmagnet` shows no errors, and that `.../cr-ab-comp-1/status.json` exists.
Then leave it alone.

## 6. During the run
- Do not restart, pause or touch either container. Do not run anything else that uses DHT on the box or the home IP.
- When something happens on the host, write it down at that moment, with the collector's note command (the time is stamped for you):
  `docker run --rm -v /mnt/user/appdata/lodestar-soak:/evidence -v /path/to/collect.py:/collect.py:ro python:3.12-slim python /collect.py note --root /evidence/cr-ab-comp-1 --category <category>`
  Categories: `import`, `other_crawler`, `array_operation` (parity, rebuild, mover), `host_reboot`, `plugin_container_action`, `link_outage`, `router_outage`, `public_ip_change`, `power_event`, or `other` for a note that changes nothing. A note written late, or after you looked at data, does not count.
- Do NOT open `samples.jsonl`, the `metrics/` folder or the `.prom` files. `status.json` shows progress only; that is fine to read.
- If something goes wrong, `touch /mnt/user/appdata/lodestar-soak/cr-ab-comp-1/stop.request`. The run is then invalid and is repeated from fresh data (CR-SPEC 6.7). Never restart into the same folder.

## 7. Length and the end
- Total 25 h: 60 min warm-up, then 24 h measured. Planned end = start + 25 h (UTC) ____.
- When the collector finishes it stops bitmagnet itself (`--stop`). Check `result.json` exists, then `docker stop bitmagnet-postgres`.
- Keep all evidence. Keep the Postgres folder until the runner says the analysis is done; do not reuse it for any later run.
- No Lodestar window may start within 12 h after the end. Earliest next Lodestar start (UTC) ____. Then re-enable parity, mover and auto-updates as you normally have them.

## 8. What to send the runner
Send only these, as text:
1. The bitmagnet start time (UTC) from `docker inspect`.
2. The two digests from step 2, with tags, and the output of `docker image inspect` showing them.
3. The analyzer output, produced by the runner's command or this one, from the repo:
   `python3 docs/soak/tools/analyze_cr.py segment /mnt/user/appdata/lodestar-soak/cr-ab-comp-1 --output seg.json --publish bundle/`
The raw evidence folder stays private on the box. Do not send it, `samples.jsonl` or the logs.

## Settled by the runner (2026-11-07T00:45Z)
- **Version:** bitmagnet v0.10.1 (the highest non-beta tag; `latest` and `v0.10` share its digest). If a newer stable appears before the run, keep v0.10.1: the comparator is pinned, and changing it later means re-running it.
- **TMDB off:** see section 3.
- **`--commit`:** the image's revision label (3a.1). **`--spec-commit`:** `c011f8a679e80875e5369b56328bc1376fee2da7`, the commit of `docs/decisions/2026-11-02-CR-SPEC.md` on `main`.
- **Backup:** `not_applicable`, because Appdata Backup is disabled. Auto-updates are `excluded` with their next run time.
- **Worker keys, the 180 s first count, `--stop` and the `/proc` mount:** checked by the rehearsal (3a.3) before the real run.
- **The note command** runs in a container, because stock Unraid has no python3.
