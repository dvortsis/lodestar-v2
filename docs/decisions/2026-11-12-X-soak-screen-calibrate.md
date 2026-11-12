# X-soak-screen-calibrate decisions (the in-run screen against r4 and r5)

Runner brief X-soak-screen-calibrate, 2026-11-12. Under owner §2.62, testing comes first. This is a runner call under §2.26 and §2.33, flagged to the owner under §2.19. It changes the screen of `docs/decisions/2026-11-07-X-soak-screen.md` and the thresholds in `docs/soak/screen-thresholds.json`. Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.

**The screen stays advisory.** The CR-0 gate (`2026-10-30-CR-0-acceptance.md`), `MEMORY_FIT`, its numbers and its verdict are unchanged. A trip means "stop and investigate", and `ok` never means pass. `test_screen.py`'s gate-identity tests still prove that the report minus `screen` is identical with the screen on or off, for every fixture in `test_analyze_cr0.py`.

## Why
After #562 merged, the owner replayed the analyzer over r4's and r5's private run directories (`2026-11-08-runner-followups-3.md`). The screen got both runs wrong:
- **r5 (a pass) tripped** on `mimalloc_committed_mib_per_1000_stored`, which read 37.79 against a threshold of 20.
- **r4 (a fail) read `not_available`.** `mmap_cache_excess[main]` was absent, and one absent input held back the whole checkpoint. None of the inputs r4 did have tripped, because its growth was in memory the screen didn't read.

## What changed
- **D1. Mimalloc growth is information only.** `mimalloc_committed_mib_per_1000_stored` is still computed and shown at each checkpoint, with status `info` and `"info": true`. It never trips, and never makes a checkpoint ok or not_available. In the thresholds file it has `"trips": "never"` and no `value`, and the loader refuses it with either a threshold or `"above"`.
  - **Why demote it rather than retune it:** a pass and a fail don't separate on it. The passing r5 read 37.79 while mimalloc filled its committed heap early in the run, and was flat at 568 MiB over hours 12–24. r4's growth was outside mimalloc (D2). A threshold above 37.79 would only see a leak faster than r5's own fill, and the evidence has no such leak.
  - **The level bound stays:** `mimalloc_committed_mib` still trips above 1,536 MiB, 2.7 times r5's 568.
- **D2. A new input: anonymous memory outside mimalloc, per 1,000 stored** (`anon_outside_mimalloc_mib_per_1000_stored`). It's the OLS slope of `mem_rss_anon_bytes − mem_mimalloc_committed_bytes` against stored torrents. Both gauges are already scraped. In musl builds with `override`, mimalloc holds the Rust and C heaps, so what's left is thread stacks and mappings outside any allocator: near flat. r4's C heap (zig's musl malloc, before `override`) grew there. If either gauge is absent, the input is absent and named, like any other. Off Linux there's no `RssAnon`, so Windows runs name it absent. The input assumes the musl release build with `override`, the soak's Docker image. In a glibc build from source, SQLite's and the other C heaps sit outside mimalloc and count here.
- **D3. Absent inputs are named, and the rest still decide.**
  - Any trip still trips the checkpoint.
  - Otherwise the checkpoint is ok when at least one judged counter is ok and every other one has no reading: `absent`, `absent_at_checkpoint`, `too_few_samples` or `too_little_stored`. Those counters go in the checkpoint's `without` list, and its line reads `ok(without absent:mmap_cache_excess[main], …)`. A trip records `without` in the JSON too. Its line is unchanged.
  - A checkpoint is `not_available` only when no judged counter has a reading, named as before by the first one (`not_available(absent:…)`).
  - **`unusable` still holds back ok.** A slope too large to compute is an input that's there but can't be judged, which is a reason to look, not a missing gauge. The reason then names the unusable counter, ahead of any absent one.
  - **Checkpoint-level gaps are unchanged.** `not_reached`, `no_sample_at_checkpoint`, `identity_change` and the threshold-file states are about the run, not one input, and still give `not_available`. The overall line is ok only when every reached checkpoint is ok, and it lists the union of their `without` entries.
- **D4. The two copies stay one.** The change is in the screen-core block, kept byte-identical in `analyze_cr0.py` and `linux/collect.py` (`test_screen.py` checks it). The analyzer's `SCREEN_PREFIXES` also keeps `lodestar_mem_rss_anon_bytes` lines. The collector passes the whole snapshot, so it needs no prefix change. The Windows collector is untouched.
- **The tools README** (`docs/soak/tools/README.md`, runner-approved): "What it watches" lists the new input and shows mimalloc's growth as information. "Reading it" explains `ok(without …)` and `unjudged(…)`.
- **D5. A checkpoint with no memory input is `unjudged`, not ok** (runner, 2026-11-12T22:48Z, under §2.33: absence of evidence isn't a pass).
  - **What counts as a memory input:** the 128–512 KiB mapping growth and level, mimalloc's committed level, and memory outside mimalloc.
  - **The rule:** when none of those has a reading and nothing trips, the checkpoint reads `unjudged(no memory inputs: absent:…, …)`, naming them. It's never ok: the overall line is ok only when every reached checkpoint is ok, and otherwise it shows the first checkpoint that isn't.
  - **What doesn't change:** a trip on a structural counter still trips. A checkpoint missing only some memory inputs stays `ok(without …)`. The format follows the screen's other lines (`unjudged(…)`, no space).
  - r4's own rc.3 evidence (below) and r3-era runs read `unjudged`.

## The thresholds and their basis
Only committed aggregates are used: `2026-11-05-soak-r4-verdict.md`, `2026-11-06-soak-r5-verdict.md`, `2026-11-05-X-mimalloc-override.md`, the 37.79 in `2026-11-08-runner-followups-3.md`, and the harness figures in `2026-11-07-X-soak-screen.md`. No per-checkpoint replay values reached the runner, and raw soak data stays private.

| Counter | Threshold | Basis |
| --- | ---: | --- |
| mimalloc committed, MiB per 1,000 stored | none (information) | r5 read 37.79 (a pass); r4's growth was outside mimalloc |
| Anonymous memory outside mimalloc, MiB per 1,000 stored | 8 | r4's growth estimated at about 22.9: 2.9 times the threshold. r5 estimated at about 1.24: 6.5 times below it |

These are estimates, not readings of this input. No run has recorded it yet.

**r4's estimate:**
- r4's anonymous memory grew about 47 MiB in 1.9 h, and about 63% of that was outside mimalloc: 47 × 0.63 = 29.6 MiB.
- At about 680 stored an hour, 1.9 h is 1,292 stored, so the rate is 29.6 / 1.292 = 22.9 MiB per 1,000 stored.
- A second route agrees: about 0.2 new 128–512 KiB mappings per stored torrent, at about 120 kB dirty each, is 24 kB per torrent, or 22.9 MiB per 1,000.
- On the gate's own figure, r4's private slope of 29.35 MiB/h at 680 an hour is 43.2 MiB per 1,000. 63% of that is 27.2, so 22.9 is the lower of the two estimates.
- **The 63% is "outside mimalloc's arena" as counted in smaps, not `RssAnon − committed`.** `2026-11-04-X-mem-gauges.md` records that this difference is an approximation:
  - Transparent huge pages faulted into mimalloc's arenas inflate it. That's a false-trip path on a host where X-thp-off doesn't take effect; r4 had AnonHugePages, r5 had 0.
  - mimalloc counting a slice as committed before its pages are resident makes it read low.
- **r4's `/proc` reads came late in its baseline,** about hour 17 (`2026-11-04-X-thp-off.md`). The screen's windows are hours 0–3, 0–6 and 0–12, so the early rate isn't measured.

**r5's estimate:**
- Over hours 12–24 r5's RssAnon rose 0.86 MiB/h while mimalloc's committed bytes were flat.
- At about 695 stored an hour that's 0.86 / 0.695 = 1.24 MiB per 1,000.
- With `override`, the harness held 0.7–0.8 MiB outside the arena from 1,000 to 10,000 torrents: about 0 growth.

**Why 8:**
- It's at least 1.5 times r5's figure, the margin #562 asked for, and it trips r4's shape at hours 3, 6 and 12 by a margin of 2.9.
- 8 MiB per 1,000 is about 5.5 MiB/h at r5's rate, above the gate's 2.0 MiB/h bar. As before, slow growth below the in-run thresholds is the gate's to see, not the screen's.
- **What isn't known:** r5's hours 0–12 weren't published. While mimalloc fills, committed bytes can rise faster than resident ones, which pushes this input down, not up; a fixture shows that reads ok (`test_an_early_fill_ahead_of_residency_reads_low_not_high`). The r5 fixtures otherwise build RssAnon as committed plus a slow rise, so their ok is by construction and documents the assumption rather than proving the threshold. A future screened soak on rc.4 or later measures it.

The other thresholds are unchanged. `screen-thresholds.json` changes, so its SHA-256 is new: `f08ffdb1c2e6a8733b677d299e281d304a9a6e40efa58a3f70f87269b9b7d292`. The next soak freezes the new file and pins the new `analyze_cr0.py` and `collect.py` hashes, as for any tools change.

## The fixtures and tests
Every value in the tests is invented and shaped by the figures above. No owner soak data is committed.
- **`test_analyze_cr0.py::ScreenCalibrationTests`:**
  - **An r5-shaped pass:** mimalloc fills at 37.79 per 1,000 to hour 4, then is flat. It reads `ok` at hours 3, 6 and 12, with the fill shown as `info`.
  - **An r4-shaped trip:** r4's memory behaviour on a binary with the memory gauges (rc.4 or later). It has no mapping gauge and no main-index segment count, as in r4's replay, and memory outside mimalloc at 22.9 per 1,000. It trips `anon_outside_mimalloc_mib_per_1000_stored` at every checkpoint, and names `mmap_cache_excess[main]` in `without`.
  - **r4 as rc.3 recorded it:** no memory gauges at all. The test pins the `unjudged(no memory inputs: …)` line the replay will print, at each checkpoint and overall.
  - **`unjudged`'s edges:** one memory input left gives ok, a structural trip still trips, and an unjudged checkpoint beside ok ones keeps the overall line from reading ok.
  - **The early fill, the two other no-reading states and `unusable`:** an early fill ahead of residency reads ok. `too_little_stored` and `absent_at_checkpoint` are named, not blocking. An unusable reading beside an ok one gives `not_available(unusable:…)`.
  - **Missing inputs:** a case for each of the ten raw inputs (stored, the pool's keys, both indexes' cache entries and segments, the mapping gauge, mimalloc's committed bytes, RssAnon and held bytes). r5's shape stays ok and names exactly the counters that input feeds. r4's shape still trips, unless the dropped input is one its trip needs.
  - **Every input absent:** `not_available`.
  - **Through the analyzer:** both shapes, end to end.
- **`test_screen.py`:**
  - The fixtures gain RssAnon, and r4's gains its growth outside mimalloc.
  - The tests for snapshots without the new gauges and for Windows-shaped runs now expect `ok(without …)`. So does the r3-era run, which has only the pool's key count.
  - A run with nothing usable stays `not_available`.
  - Information counters are tested never to trip, and never to decide a checkpoint alone.
  - The loader's refusals cover a threshold on an information counter, and `"never"` on a judged one.
- **`linux/test_collect.py`:** the collector's screen fixture gains RssAnon, so its clean run still reads `ok`.

## After merge
1. **The owner replays r4 and r5 with the new analyzer and relays only the `screen:` lines.** The runner puts that request to the owner.
   - **r4 can't trip on this input.** It ran rc.3, and X-mem-gauges (`mem_rss_anon_bytes`, `mem_mimalloc_committed_bytes`) first shipped in rc.4 (`2026-11-05-soak-r4-verdict.md`, "What follows"). Its replay is expected to read `unjudged(no memory inputs: …)` (D5). The trip on r4's growth is proven only by the fixture of its shape on a gauged binary.
   - **r5** should read `ok`. If its outside-mimalloc growth reads within 1.5 times of 8, a follow-up retunes it under a new hash.
2. **Precondition 1 of `2026-11-07-X-soak-screen.md` still holds:** read the mapping gauge for about an hour on the musl release build with `override`. The in-run screen advises only until both steps are done.
