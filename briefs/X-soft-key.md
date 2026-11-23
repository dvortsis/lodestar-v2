# X-soft-key: a `soft` treatment key for fleet A/B blocks

Launched by the build runner on 2026-11-23, for the cloud-test lane (X-test-vm). The owner's rule for 2026-11-23T06:09Z allows new slices only when cloud testing or the API lane needs one. The owner said at 21:45Z: "I prefer identifying solutions/ issues quickly so we can proceed faster."

## Why
- X-test-vm's on-only SOFT_TIMEOUT screen is the strongest single-switch lead so far:
  - about 23.9 stored/min unstalled, against cross-run leads of about 16;
  - the late ÷ soft ratio is 0.003–0.005.
- The fleet tooling can't run it as an A/B yet, because there is no `soft` TREATMENT key. X-test-vm also wants to stack soft on ADM.

## What to build
1. **Add a `soft` treatment key** that maps to the crawler switch `soft_timeout` (`LODESTAR_DHT_SOFT_TIMEOUT`), everywhere the keys are listed:
   - `infra/fleet/steps/vm/block-lib.sh`: `TREATMENT_KEYS`, `STACKED_KEYS` and `switches_of`;
   - `infra/fleet/steps/rules.py`: `TREATMENTS`, `STACKED` and the comment on line 15;
   - `docs/soak/tools/linux/collect.py`: `TREATMENT_SWITCHES` and `STACKED`.
   `analyze_cr.py`'s `--treatment` choices follow from `TREATMENT_SWITCHES`.
2. **`soft` is stacked:** it can take WINNERS (for example `TREATMENT=soft WINNERS=adm`), like lc, hint and split. It also stays valid as a plain treatment against the shipped defaults.
3. **analyze_cr soft report.** When the treatment is `soft`, report per arm, over sampling minutes only:
   - `dht_soft_timeouts_total` and `dht_soft_timeout_late_replies_total` per hour;
   - late ÷ soft;
   - soft ÷ peer-lookup queries, if that counter exists.
   Follow the pattern of the `split` report (CR-COL-6): report it, never decide on it. Make sure collect.py keeps both counters (docs/metrics.md around line 839); add them to KEEP if they are missing.
4. **Tests:**
   - block-lib and rules accept `soft`, alone and with `WINNERS=adm`, and refuse `soft` as a winner of itself;
   - collect.py has the mapping and STACKED;
   - analyze_cr gives the soft report on a simulated run, both arms, and no soft report for other treatments.
   Follow the existing tests in `infra/fleet/tests/test_block.py`, `docs/soak/tools/linux/test_collect.py` and `docs/soak/tools/test_analyze_cr_col6.py`.
5. **Docs:** one line in `docs/operations/cloud-fleet-runbook.md` naming the new key, and a decisions file `docs/decisions/2026-11-23-X-soft-key.md`.

## Owns
- infra/fleet/steps/vm/block-lib.sh, infra/fleet/steps/rules.py, infra/fleet/tests/test_block.py
- docs/soak/tools/linux/collect.py, docs/soak/tools/linux/test_collect.py
- docs/soak/tools/analyze_cr.py, docs/soak/tools/test_analyze_cr_col6.py (or a new docs/soak/tools/test_analyze_cr_soft.py)
- docs/operations/cloud-fleet-runbook.md, docs/decisions/2026-11-23-X-soft-key.md
- runner/slices.yaml: your own new entry only (id X-soft-key, kind core, model opus, deps [CR-SOFT-TIMEOUT]), plus a placement rule if none covers it. Run `cd tools/status-connector && npm test`.

## Rules
- **No cloud calls,** no terraform and no AWS tools. X-test-vm runs the fleet; you only change the repo.
- **Don't change any crawler code** or switch default.
- **No fixed sleeps in tests.** Never skip a test.
- **Keep it small:** about 200–400 changed lines.
- **Branch and PR:** branch `slice/X-soft-key`, PR titled `X-soft-key: a soft treatment key for fleet A/B blocks`. Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.
- **Before pushing:**
  - `python3 -m unittest` in docs/soak/tools and docs/soak/tools/linux;
  - `infra/fleet/run_tests.sh`, or the fleet tests the repo uses. ssh-keygen may be missing in the sandbox, in which case compare against main.
  - `shellcheck` on block-lib.sh if installed.
- **When CI is green,** message the runner `ready <full sha>` with send_message to session_01EYhgh4JX3AmHzKXPjYCoUg.
- Commits and PRs carry no model identifiers. Use UTC Z times only.
