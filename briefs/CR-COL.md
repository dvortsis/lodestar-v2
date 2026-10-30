# CR-COL: Linux/Docker soak collector
**Model:** Sonnet (helper; Python tooling following an existing pattern). No crawler, store or network code.

## Base and PR timing
- Branch `slice/CR-COL` from `origin/land/L5`. That landing range carries the hardened Windows collector and its tests. Its PR goes to `main` after L5 merges; the runner tells you when. Until then: build, test, self-review, push, and message the runner "ready, waiting for L5". Then merge `origin/main` once (merge, not rebase) and open the PR.
- `main` was rewritten on 2026-10-30. Never use an older clone.

## Why
The v0.5.1 release gate (build-plan §0.4, `docs/decisions/2026-10-30-master-plan.md`) needs a **24 h Linux Docker soak on the owner's Unraid box**, including a ≥ 2 h gluetun leg. Today only the Windows collector exists (`docs/soak/tools/cr0-soak.ps1` plus its common, signal and rehearsal scripts). The offline analyzer `analyze_cr0.py` reads its evidence.

## Do
1. **Write `docs/soak/tools/linux/collect.py`.** It uses Python 3.10+ and the standard library only. It collects aggregate evidence from a Lodestar instance in Docker or native Linux, and writes **exactly the evidence contract `analyze_cr0.py` reads**:
   - `run.json`;
   - `samples.jsonl`, one row per sample with monotonic `elapsed_seconds`;
   - `metrics/NNNNN.prom` snapshots;
   - `result.json` on termination;
   - phase labels for startup suppression, warm-up and baseline.

   Mirror `cr0-soak.ps1`'s fields and semantics, reading it and `analyze_cr0.py` closely. Any field Linux can't provide is recorded as absent, never invented.
2. **Process and identity.**
   - Docker mode: the container ID, image digest, restart count, `docker stats`-equivalent memory and CPU from cgroup v2 files (or the Docker API socket if mounted read-only), and the health endpoint.
   - Native mode: the PID plus start time identity, never a bare PID, and RSS from `/proc`.
   - Record the host network mode, and whether a gluetun container is the network parent.
   - Record IPv4 and IPv6 counters separately when the metrics expose them.
3. **Safety, the same as the Windows collector's.**
   - It is read-only towards the instance. It never kills or signals a process unless an explicit `--stop` is given, and then only after verifying its identity.
   - It refuses a non-empty run directory.
   - It writes atomically (temp file plus rename).
   - Evidence is aggregates only: no logs, torrent names, infohashes, peer IPs or tokens.
   - It handles SIGTERM and SIGINT by writing `result.json` with the stop reason.
4. **Tests in `docs/soak/tools/linux/test_collect.py`,** with `unittest` and no network. Cover the same scenarios the Windows rehearsals cover (`test_cr0_collector.ps1`, `test_cr0_rehearsal.ps1`):
   - fault injection (health down, metrics missing or malformed, a gap);
   - a clean shutdown and an interrupted one;
   - an identity change (the container restarted, the PID was reused): the run is flagged, never stitched;
   - evidence publication (no forbidden fields);
   - and that `analyze_cr0.py` reads the output end to end.

   Use a fake HTTP server on 127.0.0.1 with the port held for the test's lifetime, and fake cgroup and `/proc` trees in temp dirs. No fixed sleeps: wait on conditions with a deadline of 10 s or more.
5. **Write `docs/soak/tools/linux/README.md`:** how to run it on Unraid next to the Lodestar container (as a one-shot container or on the host), with the gluetun leg, and where evidence goes. Keep it plain; it's a developer and operator doc.

## Own
`docs/soak/tools/linux/**` only. If the CI job for the soak tools tests needs a path added (L5 adds it to `.github/workflows/ci.yml`), say so in the PR description instead of editing CI.

## Done when
- `python3 -m unittest discover -s docs/soak/tools/linux -p 'test_*.py' -v` passes.
- The existing `python3 -m unittest discover -s docs/soak/tools -p 'test_*.py'` still passes.
- `analyze_cr0.py` produces a report from a fixture run.
- The fresh-reviewer self-review (steward §1) is done.
