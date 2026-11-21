# X-filter-budget-remeasure decisions

Runner brief X-filter-budget-main, 2026-11-21, and its follow-up: the `filter budget (§20.8)` job was red on `main`. The diagnosis found no slower code, so the runner made this slice: make the gate robust to a run of whole cases that a shared host slows alike. This is a measurement call the runner decides under `docs/owner-decisions.md` §2.26, as a refinement under §2.19 that it flags to the owner.

**Unchanged:**

- the owner's 50 µs per torrent at 10,000 rules (§1.6.6, architecture §20.8);
- every case, held or reported, and the rule counts;
- the calibration workload, its scaling, `ENGINE_SPEED`, `CALIB_*` and `REFERENCE_RATIO`;
- the drift and spread checks and the 0.4–2.0× range;
- the CI job.

Only what happens to a held case still over budget at the end of an attempt changes. That supersedes one rule in `2026-11-05-X-filter-budget-noise.md`: "a steady case over budget fails the attempt at once".

## Diagnosis: noise, not the code

- **No code change.** `crates/filter` is unchanged since 558a9121 (#957's head, 2026-11-15). So is everything it calls from metainfo, search (the tokenizer and query), classify and release-parser. The `Cargo.lock` diff since then touches only email-encoding, lettre and a flate2 line for sources-feeds, none of them in the filter's tree.
- **No toolchain change.** #957's jobs and #1007's first head all ran rustc 1.100.0 (0ef1bc386 2026-11-09).
- **The red has a shape: a run of whole cases about 1.5× slower.**
  - **#1007's first head (EPYC 9V74, red):**
    - the cases up to the mixed set ran at #957's green speed (mixed 34.2 µs here, against 34.3);
    - every case from the mixed set at commit to the pending versions' 1% at commit ran about 1.6× slower: Near 29.7 → 46.8 µs, Negation 27.8 → 43.9, Fuzzy 35.8 → 58.6, Title 28.7 → 45.8;
    - the cases after the block ran at green speed again (the pending versions' mixed set at 34.9 µs, against 34.8).
    - All eight held failures (55–73 µs) are inside the block.
  - **#957's first attempt (EPYC 9V45, red, at 558a9121):** the same shape. "1% on common words" ran 20.7 µs, and from its at-commit case the times rose: "10%" and "25%" ran about 31 µs against about 21, then Near was back at 22.5. Its re-run passed, with the mixed set at 44.6 µs.
  - **Why the gate trusted both:** each slowed case was steady within itself (third-fastest pass under 2% over its fastest), so the spread and jitter checks passed it. The calibrations run before the first case and after the last, so they saw a steady host.

| Point                | Code                                    | CPU  | Mixed set (reference µs)                     | Result                        |
| -------------------- | --------------------------------------- | ---- | -------------------------------------------- | ----------------------------- |
| #957's first attempt | 558a9121                                | 9V45 | 42.7 (block on the 1–25% cases)              | red, 3 held cases at 50–52 µs |
| #957's re-run        | 558a9121                                | 9V74 | 44.6                                         | green                         |
| #1007's first head   | `main` plus a change outside the filter | 9V74 | 43.5 (block from the mixed set at commit on) | red, 8 held cases at 55–73 µs |

- **Local A/B, the same machine and build settings:** 558a9121's and `main`'s release test binaries, alternated five runs each (13 attempts each). Medians of the third-fastest pass:
  - calibration 10.8 µs against 10.8;
  - mixed at commit 54.5 µs against 54.9;
  - the pending versions' mixed set 53.5 µs against 53.3.

  There is no difference. Both fail here: this unlisted Xeon @ 2.10 GHz runs the engine about 1.25× slower relative to the calibration than a 7763 does, so it says nothing about CI.

- **Headroom wouldn't fix it.** The mixed set reads about 44 µs on the reference, and 44 × 1.6 is still over 50.

## The change: re-measure a held case still over budget

- **The rule** (`budget`, `remeasured`):
  - An attempt that ends with held cases over budget doesn't fail at once. Each such case is measured once more, at the end of the run, on its own: set up as in the full run (`measure(Some(case))`) and timed the same way (a warm-up, then seven passes, the third-fastest decides).
  - A fresh calibration runs either side of it, as the attempt's do, never beside filter state.
  - The case fails only if it's over budget again.
- **Strictness:**
  - The second measurement is scaled by the faster of the attempt's calibration and the fastest calibration beside it, so it never reads more leniently than the first. Noise can't make a calibration faster, and it only adds time to a case.
  - A property test, `re_measuring_never_reads_more_leniently`, checks this for any calibrations, any noise and each kind of model.
  - The busy-attempt retry from `2026-11-05-X-filter-budget-noise.md` is unchanged and comes first. A re-measure only happens when an attempt would have failed.
- **Visible in the log:**
  - The gate names every re-measured case with both numbers, the attempt's calibration and both calibrations beside it.
  - A pass after a re-measure prints `::warning::passed on re-measure: …`, which shows on the check.
  - A failure lists both measurements of each case.
- **Why not scale each case by a calibration beside it** (the runner's alternative):
  - That calibration would run next to filter state, so a regression that slows the caches could slow its own scale and hide itself. The gate keeps the calibration away from filter state for that reason.
  - It would give the 7763, which reads raw, no protection at all.
  - Each case would rest on one calibration of seven passes instead of the fastest of fourteen, and noise in those passes loosens the gate.
  - Re-measuring keeps every case's reading as it was and only adds a second chance under the same scaling or a stricter one.
- **Cost:** about 1–3 s per case over budget, and only on a run that has one.
- **False red now:** a case must read over budget in its attempt and again on its own, minutes apart, with its scale never loosened. A real regression reads over both times.

## Tests

- **Unit tests** in `crates/filter/tests/props.rs`:
  - `a_slowed_block_passes_on_re_measure`: #1007's first head's mixed set at commit, 52.3 µs here, re-measured at its green speed, passes;
  - `a_steady_slowdown_fails_on_re_measure`: the engine 1.5× slower, with any calibration beside it, fails;
  - `a_faster_calibration_beside_the_re_measure_scales_it_stricter`;
  - the property test above.
- **Mutation checks, local and not committed.** A busy-wait of 30 µs per torrent was added to every mixed-set case. The budget was set to 70 µs for the check only, because this unlisted CPU reads about 55 µs on the mixed set.
  - **Slowed every time** (a steady regression): all 9 held mixed-set cases were over in the attempt (80–115 µs) and over again on re-measure (84–118 µs), and the gate failed with both numbers for each.
  - **Slowed in the full run only** (a slowed block): every case whose re-measure ran on a steady host read its own time again (52–61 µs) and printed `passed on re-measure`. The cases whose re-measure fell in this container's own slow stretches stayed over (77–90 µs) and failed. Each had calibration passes of 15–17 µs beside it, against the attempt's fastest 10.5 µs, and the gate didn't loosen its scale to excuse them.
- `cargo test -p lodestar-filter` and the props tests pass.

## Owns

`crates/filter/tests/props.rs` (the budget test and its helpers), this file, and this slice's entry in `runner/slices.yaml` with its placement line in `runner/dashboard.yaml`. The slice is named X-filter-budget-remeasure because `X-filter-budget-noise` is the 2026-11-05 slice's id.
