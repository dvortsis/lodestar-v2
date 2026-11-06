# Soak r5 verdict: rc.4 passes the memory gate (runner, 2026-11-06)

**Verdict: PASS.** The runner recorded it from the analyzer's own output, after the acceptance review that `collector_window_complete_review_required` asks for (below). The gate rule is in `docs/decisions/2026-10-30-CR-0-acceptance.md`: gate evidence yes, and the upper 95% bound of the `private_bytes` slope over baseline hours 12–24 at or below 2.0 MiB/h.

## The run
- **Build:** v0.5.1-rc.4 (`dba1a5c0f94dddfe68e0965de54ac6f3c5ed1c41`, tag `v0.5.1-rc.4`), Linux Docker on the owner's Unraid host. It was started from the rc.4 image: `run.json` records image digest `sha256:4d9d611a…cb6f3` and binary SHA-256 `1cd506c1…3a238`.
- **Protocol:**
  - 6 h of sampling suppression, a 30 min warm-up, then a 24 h baseline;
  - a frozen image, a fresh data directory (`/mnt/cache/appdata/lodestar-r5`), intensity `balanced`, Appdata Backup disabled;
  - not stitched;
  - the baseline ran from 2026-11-05T13:12:52Z to 2026-11-06T13:13:07Z (`status.json`).
- **Collector:** `run.json` records `collector_sha256` `fa718caa152c7035970d3eef6b3cbc846d2f1b7833934fd7daa42c2db0d0582f`.
  - The runner checked it: it's `docs/soak/tools/linux/collect.py` as last changed at `ac905457`. That file is unchanged at the r5 pin, `6cf0519c`, and on `main` today.
  - `run.json` has no git-revision field, so the hash is the provenance.
- **Analyzer:** `docs/soak/tools/analyze_cr0.py`, sha256 `5260624808fbb4fe17f413e1439867ed02b2692089d881df7ec84f5585d618ab`, the same one used in the provisional runs.
  - It ran with `python3 -I` in `python:3.12-slim` with `--network none` and `--c-allocator mimalloc`, with the evidence mounted read-only, on a `cp -a` copy of the run directory. The copy was deleted afterwards and the original is untouched.
  - No truncation: the fit runs to the last sample, seq 1827.
  - The evidence stays private on the owner's host.

## The analyzer's lines
- Gate evidence: yes.
- Stop reason: `baseline_window_complete`.
- Clean shutdown: yes, exit code 0 in 1.31 s.
- Evidence issues: none detected.
- Restarts: 0.
- **Memory gate:** `private_bytes`, baseline hours 12–24, 719 samples, window complete. **OLS slope 0.91 MiB/h, 95% CI [0.47, 1.35]: meets the bar.** Peak private memory was 356.55 MiB, and no sample had a working set over 1 GiB.
- **Baseline phase:**
  - 1,440 samples over 24.00 h, with valid coverage at 100.02%;
  - 0 health failures and 0 metrics failures;
  - peak working set 433.88 MiB.
- **Attribution over hours 12–24:**
  - RssAnon slope 0.86 MiB/h;
  - RssFile flat;
  - mimalloc committed flat at 568 MiB;
  - AnonHugePages 0.
- **The index:** the mmap cache tracks live files (main 24, files 72 at the end; the folders hold 34 and 82 files). File-index segments ended at 15, with a peak of 21.
- **The crawler, baseline:**
  - about 695 stored per hour;
  - writer and search errors 0;
  - backpressure at capacity as in earlier runs (peer requests 32/32, sampler states 200,000/200,000);
  - nothing over capacity.
- **For comparison:** r4 on rc.3 failed at 29.35 MiB/h [28.75, 29.96].

## The acceptance review
- **`collector_window_complete_review_required`:** this is the analyzer's status for any run whose collector reports a complete window. It asks for this review, and isn't a defect.
  - The stop reason is the collector's own: `status.json` and `result.json` were written by `collect.py` in the evidence root.
  - The final report was written by the owner's agent with its own script after its unattended runner stopped. That script only read the evidence. The gate figures above are the analyzer's.
- **`finalization_complete: false` in `status.json`, `true` in `result.json`:** expected. `complete_evidence` in `collect.py` writes `status.json` first with the flag false, then `result.json` with it true. The same was seen in r4.
- **`stale_observations`:** expected. The analyzer calls observations stale when the last sample is more than 180 s older than the analysis time (`freshness` in `analyze_cr0.py`). This analysis ran about 13 minutes after the run stopped.
- **Provisional fits:** the early provisional fits read high, at 6.58 MiB/h over hours 12–14.6 and 1.92 [1.22, 2.63] at 20.8 h. Each was labelled provisional and none was treated as the verdict, as the r3 rule requires.

## To watch
Private memory rose from 244.55 MiB at hour 16 to 298.84 MiB at hour 24 (stored 11,795 → 17,244). The fitted slope over hours 12–24 still has room under the bar. The 1.0 soak (M8 OQ4, 24 h on the RC) should compare its hours 16–24 against this.

## What follows
- **`ext:v051-soak-verdict` holds.** E-M6, E-v06-workers, E-v06-companions and the slices that wait only on the verdict are unheld. The runner launches them at up to four slots (owner §2.42), critical path first.
- **Still needed for the v0.5.1 go:**
  - the gluetun leg on rc.4. The owner started it about 2026-11-06T15:00Z; it runs about 8.5 h.
  - the Windows MSI check on rc.4, at least 1 h: start, stop, a clean restart, stored above 0.
- **Then the owner tags `v0.5.1`** on `dba1a5c0`.
