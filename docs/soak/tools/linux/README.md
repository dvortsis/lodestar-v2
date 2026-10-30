# Linux/Docker soak collector

`collect.py` collects aggregate soak evidence from a running Lodestar instance on
Linux, in Docker (Unraid) or native. It writes the same evidence that
`../cr0-soak.ps1` writes on Windows, so `../analyze_cr0.py` reads it unchanged.
Python 3.10+ and the standard library only. Developer and operator doc.

It **observes**; it does not launch Lodestar. Start a fresh Lodestar first (new
data directory, default settings), then start the collector right after it, so the
startup-suppression phase is recorded from the beginning.

## What it does and does not do

- Read-only towards the instance. It never kills or signals anything unless you
  pass `--stop`. Then, when collection ends for any reason, it re-verifies the
  instance's identity and stops it (`docker stop`, or SIGTERM then SIGKILL for a
  native process). A changed identity is never signalled.
- Refuses a run directory that is not empty. Writes evidence atomically
  (temporary file, then rename).
- Aggregates only: no logs, torrent names, infohashes, peer addresses or tokens.
  Metrics snapshots keep only the series `analyze_cr0.py` reads, with plain
  enum-like labels. It stores chosen fields from `docker inspect`, never the
  environment or command line.
- SIGTERM, SIGINT or an empty `stop.request` file in the run directory ends the
  run with `result.json` reason `operator_stop` (and `stop_signal`).
- A field this host cannot provide is left out, never invented. Docker mode has
  no `handles` (open files). Neither mode records `redirected_log_bytes`.

## Identity

| Mode | Identity | Memory and CPU |
| --- | --- | --- |
| Docker | container ID plus `StartedAt` | cgroup v2 files (`memory.current` minus `inactive_file`, as `docker stats` does; `anon`; `cpu.stat`; `pids.current`), or the Docker API stats call if the cgroup tree isn't mounted |
| Native | PID plus boot ID plus process start time, never a bare PID | `/proc/<pid>/status` (`VmRSS`, `RssAnon`, `Threads`), `/proc/<pid>/stat` CPU, `/proc/<pid>/fd` count |

If the identity changes (the container restarted, the PID was reused) the run
ends at once with reason `process_exited_identity_changed` and an
`identity_change` entry in `result.json`. Samples from two identities are never
joined. A container that exits ends the run with `process_exited_<code>`
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

The collector needs: the Docker socket (read-only is enough unless you use
`--stop`), the host cgroup tree, a place to write evidence, and a path to
Lodestar's HTTP port.

```sh
# Evidence goes under /mnt/user/appdata/lodestar-soak. The run directory
# (run-1 here) must not exist yet; use a new name for every run.
docker run -d --name lodestar-soak-collector --restart no \
  --network host \
  -v /var/run/docker.sock:/var/run/docker.sock:ro \
  -v /sys/fs/cgroup:/host/cgroup:ro \
  -v /mnt/user/appdata/lodestar-soak:/evidence \
  -v /path/to/collect.py:/collect.py:ro \
  python:3.12-slim python /collect.py --mode docker --container lodestar \
    --url http://127.0.0.1:8484 --cgroup-root /host/cgroup \
    --root /evidence/run-1 --commit <40-char source commit> \
    --storage-path /evidence
```

- `--network host` lets the collector reach a published port at `127.0.0.1`.
  Use the address you actually publish; the URL is `http://host:port` only.
- Add `--storage-path` for every filesystem that matters (the Lodestar appdata
  share and the evidence share). The collector stops the run if free space
  drops below `--stop-free-bytes` (default 10 GB). Defaults: warn at 20 GB,
  refuse to start below 50 GB.
- Running it on the host instead is the same command without `docker run`:
  `python3 collect.py --mode docker --container lodestar --root ... --commit ...`.
- Native Lodestar: `--mode native --pid <pid>` instead of `--container`.
- Stop early and keep the evidence: `docker stop lodestar-soak-collector`
  (SIGTERM) or `touch <run>/stop.request`. Never restart into the same
  directory. Start a new one.
- Without `--stop`, `result.json` says `left_running`, so the analyzer reports
  `run_state: completion_unknown` and no clean-shutdown evidence. That is
  expected for an observer. Add `--stop` (and a read-write socket mount) if
  the final graceful shutdown should be part of the evidence. Native mode
  can't read the exit code of a process it didn't start, so it never reports a
  clean shutdown.

## The gluetun leg (at least 2 hours)

Run the Lodestar container with `network_mode: container:gluetun` (or
`--network container:gluetun`), publish Lodestar's HTTP port on the gluetun
container, and point `--url` at that published address. Add
`--require-gluetun` so the collector refuses to start unless gluetun really is
the network parent. Use a separate run directory for the leg, and record the
VPN provider and region in your own notes, not in the evidence.

## Windows-equivalent settings

The defaults match the Windows collector: samples every 60 s, a warm-up of at
least 30 minutes after suppression ends, then a 24 h baseline. The same stop
rules apply (three failed health checks or metrics reads, three capacity
violations, RSS over 1 GiB for five samples, active peer requests without
completion for five minutes, an overall deadline of 8 h plus the baseline).
`--rehearsal` allows shorter windows for testing and is recorded in
`run.json`. Rehearsal evidence is test data, not soak coverage.

## Evidence

Everything is written under `--root`:

| File | Content |
| --- | --- |
| `run.json` | provenance, identity, host, network, settings |
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

## Tests

```sh
python3 -m unittest discover -s docs/soak/tools/linux -p 'test_*.py' -v
python3 -m unittest discover -s docs/soak/tools -p 'test_*.py'
```

The Linux tests use a fake HTTP server on 127.0.0.1, fake cgroup and `/proc`
trees, a fake Docker API and a virtual clock. They need no network and no
Docker, and cover fault injection, clean and interrupted shutdowns, identity
changes, evidence contents and an end-to-end `analyze_cr0.py` read.
