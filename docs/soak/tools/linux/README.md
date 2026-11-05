# Linux/Docker soak collector

`collect.py` collects aggregate soak evidence from a running Lodestar instance on
Linux, in Docker (Unraid) or native. It writes the same evidence that
`../cr0-soak.ps1` writes on Windows, so `../analyze_cr0.py` reads it unchanged. Its report opens with a "Run" section and a `gate_evidence`
verdict, so a rehearsal or a 2 h leg isn't mistaken for 24 h evidence.
Python 3.10+ and the standard library only. Developer and operator doc.

It **observes**; it does not launch Lodestar. Start Lodestar first, then start the
collector right after it, so the startup-suppression phase is recorded from the
beginning.

## The v0.5.1 gate procedure (build-plan §0.4)

The 24 h gate soak replaces the old pre-CR-0 container **on its existing Oct 22
volume**, so it is also the upgrade test:

1. Back up the old instance's data first: `lodestar backup --to <path>` (run it the
   way you run other `lodestar` commands for that container). Keep the backup until
   the soak is accepted.
2. Stop the old container. Start the new image (the frozen rc build) on the same
   volume. Note the start time.
3. Start the collector right after Lodestar starts (command below). Sampling is
   suppressed for 6 h after start, then there is a 30 min warm-up, then the 24 h
   baseline (about 30.5 h in all).
4. Don't restart the container mid-run. A restart is an identity change and ends
   the run; runs are never stitched. A failed soak is a fix PR and a re-soak of a
   frozen build.

A run on a fresh volume (new data directory) is the secondary option, for
rehearsals or comparison.

## What it does and does not do

- Read-only towards the instance. It never kills or signals anything unless you
  pass `--stop`. Then, when collection ends for any reason, it re-verifies the
  instance's identity and stops it (`docker stop`, or SIGTERM then SIGKILL for a
  native process). A changed identity is never signalled.
- Refuses a run directory that is not empty. Writes evidence atomically
  (temporary file, then rename).
- Aggregates only: no logs, torrent names, infohashes, peer addresses or tokens.
  No personal paths or hostnames either: volumes are labelled by role
  (`evidence`, `storage-1`, ...), the URL host is stored as a class (`loopback`,
  `private`, `public` or `name`) plus the port, the image as its digest only, and
  a custom Docker network as `custom`.
  Metrics snapshots keep only the series `analyze_cr0.py` reads and the
  storage and cache sizes (`STORAGE` in `collect.py`), with plain enum-like
  labels. It stores chosen fields from `docker inspect`, never the
  environment or command line.
- SIGTERM, SIGINT or an empty `stop.request` file in the run directory ends the
  run with `result.json` reason `operator_stop` (and `stop_signal`).
- A field this host cannot provide is left out, never invented. Docker mode has
  no `handles` (open files). Neither mode records `redirected_log_bytes`.
- Docker mode: `threads` is the cgroup's task count (`pids.current`), `sha256`
  is the image ID (the analyzer's report labels it "Binary SHA-256"; `run.json`
  says `sha256_kind: image_id`), and `process_id` is the host PID Docker
  reports, or `0` if it reports none, because the analyzer requires a number.
- Start the collector within minutes of starting Lodestar. Attached to an
  instance whose suppression already ended, it never sees the transition and
  the run stays in `startup` until the overall deadline.

## Identity

| Mode | Identity | Memory and CPU |
| --- | --- | --- |
| Docker | container ID plus `StartedAt` | cgroup v2 files (`memory.current` minus `inactive_file`, as `docker stats` does, which includes page cache; `anon`, the process memory the stop rule judges; `cpu.stat`; `pids.current`), or the Docker API stats call if the cgroup tree isn't mounted |
| Native | PID plus boot ID plus process start time, never a bare PID | `/proc/<pid>/status` (`VmRSS`, `RssAnon`, `Threads`), `/proc/<pid>/stat` CPU, `/proc/<pid>/fd` count |

If the identity changes (the container restarted, the PID was reused) the run
ends at once with reason `process_exited_identity_changed` and an
`identity_change` entry in `result.json`. Samples from two identities are never
joined. If Lodestar itself restarts inside the same container (its
`lodestar_start_time_seconds` changes) the run ends with
`process_exited_instance_restarted`, before any row from the new start is written.
A container that exits ends the run with `process_exited_<code>`
(`oom_killed` is recorded). If the identity can't be read for three minutes the
run ends with `collector_error`; shorter outages are left as gaps, which
`analyze_cr0.py` reports.

`run.json` also records the network mode (`host`, `bridge`, `container` or a
network name), whether a **gluetun** container is the network parent, and the
parent's identity. If the parent is restarted the change is counted in
`result.json` as `network_parent_changes` and the run continues; the usual
three-failed-health-checks rule still applies. If the metrics label counters by
IP family (`family="v4"` or `"v6"`), each sample carries `ip_family_counters`
with the families kept apart. Without such labels the field is absent.

## Run it on Unraid, next to the Lodestar container

The collector needs: the Docker socket, the host cgroup tree, a place to write
evidence, and a path to Lodestar's HTTP port.

**The Docker socket is full control.** A `:ro` bind mount does not limit the
Docker API, so anything in the collector's container can start, stop or remove
any container. The collector only reads, and stops the instance only with
`--stop`, but treat the mount as root access. To narrow it, put a socket proxy
in front that allows only `GET` on containers and images (plus `POST` stop if
you use `--stop`), and pass its socket as `--docker-socket`.

```sh
# Evidence goes under /mnt/user/appdata/lodestar-soak. The run directory
# (run-1 here) must not exist yet; use a new name for every run.
docker run -d --name lodestar-soak-collector --restart no \
  --network host \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v /sys/fs/cgroup:/host/cgroup:ro \
  -v /mnt/user/appdata/lodestar-soak:/evidence \
  -v /path/to/collect.py:/collect.py:ro \
  -v /path/to/lodestar-password:/password:ro \
  python:3.12-slim python /collect.py --mode docker --container lodestar \
    --url http://127.0.0.1:8484 --cgroup-root /host/cgroup \
    --root /evidence/run-1 --commit <40-char source commit> \
    --intensity balanced --storage-path /evidence --password-file /password
```

- `--intensity`: pass the preset Lodestar is set to (`low`, `balanced` or
  `aggressive`). The analyzer takes the outstanding-RPC limit from it. Left
  out, `run.json` has no preset, and the analyzer assumes `balanced` and
  reports `intensity_assumed_balanced`.
- `--password-file` (recommended; optional for a CR-0 run, required for a
  CR-SPEC run): a file holding Lodestar's admin password. The collector logs in
  once at start to read the build's target triple from `GET /api/v1/system`
  and records only that, as `build_target` in `run.json`. The analyzer needs it
  to know who serves C's `malloc` (see "Memory attribution" below). If the API
  doesn't answer at start, the read is retried on later samples until it does.
  The password and the session cookie stay in memory, never on disk. The login
  is plain http, so two guards apply in every run kind, and the collector
  refuses to start if either fails:
  - the file is readable by its owner only: `chmod 600`, owned by the user the
    collector runs as (root in the container above);
  - `--url` is a loopback address (`127.0.0.1`, `localhost`) or a private IP
    address. A Docker network hostname such as `http://lodestar:8484` is
    refused, because the collector can't tell where a name leads: use
    `--network host` and `127.0.0.1`, or the container's private IP.

  Without it, `run.json` has no build target, and the report's C allocator is
  unknown unless you pass `--c-allocator` to the analyzer.
- `--network host` lets the collector reach a published port at `127.0.0.1`.
  Use the address you actually publish; the URL is `http://host:port` only.
- Add `--storage-path` for every filesystem that matters (the Lodestar appdata
  share and the evidence share). The collector stops the run if free space
  drops below `--stop-free-bytes` (default 10 GB). Defaults: warn at 20 GB,
  refuse to start below 50 GB.
- Running it on the host instead is the same command without `docker run`:
  `python3 collect.py --mode docker --container lodestar --root ... --commit ...`.
- Native Lodestar: `--mode native --pid <pid>` instead of `--container`, run on
  the host (or in a container with `--pid=host`). `--stop` signals through a
  pidfd opened at attach and re-verified against the recorded start time, so a
  recycled PID is never signalled; it is refused where pidfd isn't available
  and with a `--proc-root` other than `/proc`. Docker IDs are not reused. A
  restart between the identity check and `docker stop` is caught afterwards,
  because the exit is read back and compared with the verified identity.
- Stop early and keep the evidence: `touch <run>/stop.request`, or
  `docker stop -t 120 lodestar-soak-collector` (SIGTERM). Docker's default 10 s
  grace is too short when `--stop` is set: finalization can wait up to 90 s for
  the instance to exit. Never restart into the same
  directory. Start a new one.
- Without `--stop`, `result.json` says `left_running`, so the analyzer reports
  `run_state: completion_unknown` and no clean-shutdown evidence. That is
  expected for an observer. Add `--stop` if the final graceful shutdown
  should be part of the evidence. Native mode
  can't read the exit code of a process it didn't start, so it never reports a
  clean shutdown.

## The gluetun leg (a separate 2 h run)

Recreating the container under gluetun is an identity change, so the leg is its
own run, with its own run directory and its own timeline: 6 h suppression,
30 min warm-up, then a 2 h baseline (about 8.5 h in all). Run the Lodestar
container with `network_mode: container:gluetun` (or `--network container:gluetun`),
publish Lodestar's HTTP port on the gluetun container, and point `--url` at that
published address.

```sh
docker run -d --name lodestar-soak-collector-gluetun --restart no --network host \
  -v /var/run/docker.sock:/var/run/docker.sock -v /sys/fs/cgroup:/host/cgroup:ro \
  -v /mnt/user/appdata/lodestar-soak:/evidence -v /path/to/collect.py:/collect.py:ro \
  python:3.12-slim python /collect.py --mode docker --container lodestar \
    --url http://127.0.0.1:8484 --cgroup-root /host/cgroup --root /evidence/run-gluetun \
    --commit <40-char source commit> --intensity balanced --storage-path /evidence \
    --require-gluetun --leg gluetun --baseline-hours 2
```

`--leg gluetun` is allowed only with `--require-gluetun` (the collector refuses to
start unless gluetun really is the network parent) and permits `--baseline-hours 2`
outside `--rehearsal` (the minimum is 2). The leg is written to `run.json` as
`settings.leg` (`gluetun`, or `main` by default). Record the VPN provider and
region in your own notes, not in the evidence.

## Windows-equivalent settings

The defaults match the Windows collector: samples every 60 s, a warm-up of at
least 30 minutes after suppression ends, then a 24 h baseline. The same stop
rules apply, except for the memory basis described below (three failed health
checks or metrics reads, three capacity violations, process memory over 1 GiB
for five samples, active peer requests without completion for five minutes, an
overall deadline of 8 h plus the baseline).

The memory rule judges process memory, not page cache. Native mode uses `VmRSS`
(reason `rss_over_1GiB_for_five_samples`). Docker mode uses the cgroup's `anon`
(reason `process_memory_over_1GiB_for_five_samples`), because a container's
working set also counts the cache from its own file reads. If `anon` is missing
from a sample, that sample falls back to the working set and the old reason.
The Docker API stats on a cgroup v1 host carry no `anon`, so there every sample
uses the working set. `run.json` records the basis as
`settings.memory_stop_basis` (`anon` or `rss`). In Docker mode the working set
is still recorded every sample: `events.log` notes once when it first passes 1
GiB, and `status.json` and `result.json` count such samples in
`working_set_over_1GiB_samples`. It never stops the run.

`--rehearsal` allows shorter windows for testing and is recorded in
`run.json`. Rehearsal evidence is test data, not soak coverage.

## Evidence

Everything is written under `--root`:

| File | Content |
| --- | --- |
| `run.json` | provenance (including `build_target`, with `--password-file`), identity, host, network, settings |
| `samples.jsonl` | one row per sample, with monotonic `elapsed_seconds` and phase |
| `metrics/NNNNN.prom` | sanitized metrics snapshot for row `NNNNN` |
| `baseline-start.prom` | snapshot at the start of the baseline |
| `disk.jsonl`, `status.json`, `events.log` | storage checks, latest state, notes |
| `result.json` | why it stopped, shutdown outcome, publication failures |
| `collector.py` | the exact collector that ran (hash in `run.json`) |

Analyze it with:

```sh
python3 docs/soak/tools/analyze_cr0.py <run> --format json --output <new-report.json>
```

A report is not a pass. Review `issues`, coverage and the stop reason.

**Search commits and the mmap-cache gauge (CR-COL-4).** Snapshots also keep
`lodestar_search_commits_total` and `lodestar_search_mmap_cache_entries{index="main"|"files"}`
(live mappings retained after the latest prune; binaries without the prune don't expose it).
The report's `memory_context` adds `mib_per_1000_commits` (private MiB per 1,000 search
commits, over the gate's hours 12-24), and its `search` section gives the commit counter and,
per index, the gauge's first, last and max. Evidence without these metrics (r3 and older)
analyzes as before, with the fields absent. A memory fit over a window that isn't fully
covered prints as `provisional (window incomplete)`, with no pass or fail wording. The bar
itself is unchanged. Soak r4's record pins the collector and analyzer git revision next to
the rc.3 image digest.

**Memory attribution (X-mem-gauges).** Snapshots also keep the worker's
`lodestar_mem_rss_anon_bytes`, `_rss_file_bytes`, `_anon_huge_bytes`,
`_mimalloc_committed_bytes`, `_mimalloc_peak_committed_bytes`, `_sqlite_bytes` and
`_sqlite_highwater_bytes`, plus `lodestar_search_segments{index="main"|"files"}`. The report's `memory_attribution`
section gives each one's first, last and max and its slope per hour over the gate's hours
12-24, and "other heap". Which formula it uses depends on who serves C's `malloc`
(`docs/metrics.md`, "Memory attribution"). The analyzer takes that from `run.json`'s
`build_target` and reports it as `c_allocator`:
- **A `*-linux-musl` target** (the Docker image and the tarballs): mimalloc, through its
  `override` feature. SQLite is already inside mimalloc's bytes, so other heap is RssAnon
  minus mimalloc.
- **Any other target** (glibc, Windows): the C library. Other heap is RssAnon minus mimalloc
  minus SQLite, or minus mimalloc alone when the binary lacks the SQLite gauges.
- **No target recorded** (no `--password-file`, or an older collector): unknown. Other heap is
  RssAnon minus mimalloc, labelled as possibly including SQLite.

`analyze_cr0.py --c-allocator mimalloc|libc` states it instead. Use `libc` for a musl build
made before X-mimalloc-override (#448), which the target can't tell apart. The report
records whether the value came from the target or from you. It's informational and doesn't
change the gate. Evidence without these metrics analyzes as before, with each reported `absent`.

**Memory gate (v0.5.1).** The report's `memory_fit` section (in Markdown, "Memory
gate") fits the OLS slope of `private_bytes` (cgroup `anon`, or `RssAnon` on a
native run) over baseline hours 12–24, with its 95% CI and the run's peak. The
v0.5.1 bar is an upper bound ≤ 2 MiB/h (owner, 2026-10-30;
`docs/decisions/2026-10-30-CR-0-acceptance.md`). A restart inside the window or
a window that isn't fully covered is not judged.

## CR-SPEC crawler runs (CR-COL-3)

The comparator, every A/B arm and the CR-COMB acceptance soak are collected with
`--run-kind` and judged by `../analyze_cr.py` under
`docs/decisions/2026-11-02-CR-SPEC.md`. See `docs/decisions/2026-11-02-CR-COL-3.md`
for what is kept and why.

```sh
python3 collect.py --mode docker --container lodestar --cgroup-root /host/cgroup \
  --proc-root /host/proc --url http://127.0.0.1:8484 --root /evidence/utp-b1-s1 \
  --commit <source commit> --spec-commit <commit of CR-SPEC in force> \
  --run-kind ab --treatment utp --intensity balanced --stop \
  --password-file /secrets/lodestar-admin \
  --checklist auto_update=excluded --next-run auto_update=2026-11-10T03:00:00Z \
  --checklist backup=excluded --next-run backup=2026-11-09T03:00:00Z
```

- `--run-kind ab` measures 6 h after a 60 min warmup; `comb` and `comparator`
  measure 24 h. These are fixed outside `--rehearsal`.
- `--password-file`: the admin password, for the settings and Insights checks
  (API keys can't reach them). It's read at each login and never written.
- `--proc-root /host/proc` (mount the host's `/proc` read-only): the network
  counters. Without it they're reported as not available.
- `--stop` on every A/B segment: an arm switch must be a clean stop.
- The comparator: `--run-kind comparator --container bitmagnet
  --postgres-container bitmagnet-postgres`. It counts `SELECT count(*) FROM
  torrents` in a read-only session at every scrape. **Order matters** for the
  "store empty at start" check (rule 5.4):
  1. Create a fresh Postgres database and volume, and start Postgres.
  2. Start the bitmagnet container.
  3. Start the collector at once, in the same command line if you can. Its first count comes 5 s
     after it attaches, before bitmagnet has stored anything, and it must come within 180 s of
     bitmagnet's start (rule 6.6).

  The collector can't attach before bitmagnet starts, because it verifies the running
  container's identity. A first count above 0 voids the run.
- Set crawler switches **by environment only**. The collector refuses to start
  if Lodestar was given `--config` or its data directory has a `config.toml`,
  because switches set there would be invisible to it.
- The password file must be `chmod 600`, and `--url` must be a loopback or
  private address: the login goes over plain http.
- **Host activity:** write each event when it happens, with
  `python3 collect.py note --root <run> --category <category>`. Categories:
  `import`, `other_crawler`, `array_operation`, `host_reboot`,
  `plugin_container_action`, `link_outage`, `router_outage`,
  `public_ip_change`, `power_event` (or `other` for a note that invalidates
  nothing).
- While a window is open, `status.json` shows progress only, never counts or
  rates. Don't open `samples.jsonl` until the run ends (rule 6.11).

Judge and publish:

```sh
python3 docs/soak/tools/analyze_cr.py segment <run> --output <seg.json> --publish <bundle-dir>
python3 docs/soak/tools/analyze_cr.py effect --treatment utp \
  --block a1.json b1.json b2.json a2.json --block b3.json a3.json a4.json b4.json
python3 docs/soak/tools/analyze_cr.py gate comb.json --comparator comp.json
```

The bundle is aggregates and hourly rows only. `publish` refuses anything
address- or infohash-shaped.

## Tests

```sh
python3 -I -m unittest discover -s docs/soak/tools/linux -p 'test_*.py' -v
python3 -I -m unittest discover -s docs/soak/tools -p 'test_*.py'
```

`test_collect_spec.py` runs each CR-SPEC run kind on the virtual clock and has
`analyze_cr.py` judge it; `../test_analyze_cr.py` has a passing and a failing
fixture for each rule the analyzer judges.

The Linux tests use a fake HTTP server on 127.0.0.1, fake cgroup and `/proc`
trees, a fake Docker API and a virtual clock. They need no network and no
Docker, and cover fault injection, clean and interrupted shutdowns, identity
changes, evidence contents and an end-to-end `analyze_cr0.py` read.
