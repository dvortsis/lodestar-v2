# CR-COL-5: collector and analyzer for the r5.1 design

**Model:** Opus. **Branch:** `slice/CR-COL-5`. **PR title:** `CR-COL-5: <title>`.

## Why
The owner adopted r5 and r5.1 on 2026-11-20 (PR #970: `docs/decisions/2026-11-20-path-to-100-cr-spec.md`, `docs/decisions/2026-11-20-path-to-100.md`, `docs/build-plan.md` §0.5; read them on branch `docs/dht-throughput-plans-20261119` if #970 isn't merged yet). r5.1 amends CR-SPEC's design (`docs/decisions/2026-11-02-CR-SPEC.md` rule 6.1): from stage 2 the control arm has every earlier winner on (stacked controls, A2), and stage 2's treatment is two switches judged together (A1, CR-LOOKUP-END with CR-1c). The collector and analyzer (`docs/soak/tools/`) must record and check that. Needed from stage 2 (CR-AB-LC); **stage 1 (CR-AB-ADM, CR-AB-4) never waits for it**, so nothing you change may alter stage 1's outputs.

## What to build
1. **Per-segment switch sets.** Each segment records the full set of crawler switches on in each arm (from the run's config/`/api/status` as the collector already reads it), and the analyzer prints the control's switch set beside each result. A run whose control set differs between blocks of the same treatment is invalid, with a clear reason.
2. **The stacked control's identity.** The analyzer takes the list of earlier winners (a small input file or flag) and refuses a stage-2+ run whose control doesn't have exactly those on.
3. **A1's joint mechanism check** (r5.1 A1): a segment is void unless, in the treatment arm, lookups ending on values > 0, occupied slots exceed 32 at least once in the window, and the query rate stays within the preset. Read these from CR-LEDGER's series.
4. **CR-LEDGER's `W`, `q`, `y`.** Keep CR-LEDGER's new series in the sanitized snapshot (`KEEP`, the `SAFE_LABELS` pattern) and report `W`, `q` and `y` per arm and segment. Also keep `dht_peer_lookup_seconds_total` and `dht_peer_lookups_answered_total`. CR-LEDGER is being built in parallel: the runner will message you its exact metric names; until then, write against a clearly marked placeholder list and fixtures, and fill in the names before the PR.
5. **Older snapshots still analyze**: series absent are reported absent, never a crash. Stage 1's analysis must produce the same verdict lines as before your change (add a regression fixture from the existing CR-COL-3/CR-COL-4 tests).

## Bounds
- CR-SPEC's rules don't change except as r5.1 states. The fit, the +5 % margin, validity, the no-peeking rule (1.3) and two blocks per outcome stay as they are.
- No raw logs, IPs, hashes or host names in fixtures (sanitized only).

## Tests
- Passing and failing fixtures for each new check (switch-set mismatch, wrong stacked control, A1 void cases, absent series).
- An end-to-end fixture from emitted metric text through the sanitized snapshot to the analysis output, following `test_analyze_cr0.py`'s pattern.
- A short rehearsal on synthetic data, run in the PR and described in the decisions file `docs/decisions/2026-11-20-CR-COL-5.md`.

## Rules
- Owns: `docs/soak/tools/**` and your decisions file. Running alongside you: CR-LEDGER (DHT and metrics), X-crawler-sim-calibrate (`crates/crawler-sim/**`), GS-3. Don't edit their files.
- Before the PR: the tools' Python tests (the repo's CI command for them), ruff/format if the repo runs them.
- Your `runner/slices.yaml` entry comes with #970.
- ≲ 1,200 changed lines.
- Use the steward skill. Report `ready <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.
