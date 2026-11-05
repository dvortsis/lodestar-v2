# Soak r4 verdict: rc.3 fails the memory gate (runner, 2026-11-05)

**Verdict: FAIL (memory).** Recorded by the runner from the analyzer's own output. Gate rule: `docs/decisions/2026-10-30-CR-0-acceptance.md` (gate evidence yes, and the upper 95% bound of the `private_bytes` slope over baseline hours 12–24 at or below 2.0 MiB/h).

## The run
- **Build:** v0.5.1-rc.3 (`release/v0.5.1` at `bd9300dd`), Linux Docker on the owner's Unraid host, started from the rc.3 image digest.
- **Protocol:** 6 h sampling suppression, 30 min warm-up, 24 h baseline; frozen image, fresh data directory, Appdata Backup disabled; not stitched. Baseline ended 2026-11-05T00:55:30Z.
- **Analyzer:** `docs/soak/tools/analyze_cr0.py`, sha256 `7cf98b775cd48eb4d9fb635326ee952c5e01ff075c0d63382ee039983027972b` (equal to the pin), run with `python3 -I` in `python:3.12-slim`, no network, evidence mounted read-only, on a copy of the run directory at 2026-11-05T00:57:22Z. Exit code 0; stdout sha256 `a89ca86b3b7f512ffd6f91a1fb3c4e0d5e7457f0c541e6bb09e5260324d7fc70`. The evidence stays private on the owner's host.

## The analyzer's lines
- Gate evidence: yes. Stop reason: `baseline_window_complete`. Clean shutdown: yes. No evidence issues. Restarts: 0.
- **Memory gate:** `private_bytes`, baseline hours 12–24, 719 samples, window complete. **OLS slope 29.35 MiB/h, 95% CI [28.75, 29.96]**: misses the bar. Peak private memory 727.36 MiB; no sample with a working set over 1 GiB.
- Baseline phase: 1,440 samples over 24.00 h, 0 health and 0 metrics failures, peak working set 885.40 MiB.
- `status.json`'s `finalization_complete: false` is expected: the collector writes `status.json` before `result.json` (`complete_evidence` in `collect.py`), and the analyzer read `result.json` with no finalization issue.

## What the growth is
Read-only `/proc` reads of the worker (relayed by the dashboard session, 2026-11-04 22:34Z to 2026-11-05 00:30Z):
- Anonymous memory grew about 47 MiB in 1.9 h. About 63% of it is **outside** mimalloc's one 1 GiB arena, and almost all of that is in 128–512 KiB anonymous mappings: 1,843 → 2,085 of them, about 120 kB dirty each, about 0.2 new mappings per stored torrent.
- No `[heap]`, LazyFree 0, 32–33 threads, one `.db-shm` mapping; THP collapse counters barely moved (X-thp-off is a level fix, not the slope).
- The glibc harness on `main` (10k torrents) showed glibc's mmapped-block count flat, which points away from a leak in C code and at the static musl build's C allocator, which never returns those mappings.

## What follows
- **rc.4** (owner §2.40): X-thp-off, X-mem-gauges with X-mem-gauges-2, X-log-url-redact, and **X-mimalloc-override** (#448: mimalloc serves C allocations too on the musl build; its threaded harness shows the 128–512 KiB mappings outside the arena go to zero), plus `LEGAL-1r`. Brought onto `release/v0.5.1` by `R-rc4-cut`.
- **Soak r5** on the frozen rc.4 image, same protocol and bar. r4's evidence does not carry over. The gluetun leg and the Windows MSI check move to rc.4 as well.
- On another fail, the same loop: fix on `main`, a new rc, a fresh frozen soak; never stitched.
