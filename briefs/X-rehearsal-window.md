# X-rehearsal-window: the round-1 rehearsal runs a 3 h window

**From:** the runner, 2026-11-28T00:28Z. **Model:** Opus. **Branch:** `slice/X-rehearsal-window`. **PR title:** `X-rehearsal-window: The round-1 rehearsal runs a 3 h window`.

**Why (runner, CR-SPEC-R1):**
- The plan (§6.3) and ruling 11 let the rehearsal run up to a 4 h cap, so every arm can reach 60 active minutes and drop-c can decide.
- Today the coordinator starts the rehearsal's collectors with `--baseline-hours 2` (`infra/fleet/steps/vm/round1-coordinator.py` around 616–617). Add the 30 min warm-up, and `watch` stops every arm at about 2 h 30 min plus 2 min. A slow arm, or a drop-c `not_judged`, therefore has no room to extend.
- The runner's ruling: the rehearsal uses a 3 h window. Staggered starts then end it at about 3 h 35 min, inside the 4 h cap.
- The operator may still end it early with `stop` once drop-c says keep or drop.

**The change:**
1. In the rehearsal branch of the collector argv, change `--baseline-hours 2` to `3`. Also fix the comment ("the 2 h rehearsal") and any usage or docstring text that says 2 h.
2. Check that nothing else assumes a 2 h rehearsal, and fix whatever does:
   - `collect.py`'s rehearsal handling;
   - `analyze_cr.py`'s rehearsal checks (`window_hours`);
   - `drop_c`'s interval;
   - the round-1 fixtures and the e2e test;
   - the coordinator's `watch` end time.
3. Make sure the window still can't exceed the 4 h cap:
   - Compute the worst case from the stagger the coordinator uses, and state it in the decisions file.
   - If the worst case can pass 4 h, have `watch` stop every arm at the cap measured from the first start, and add a test for that.
4. **Tests:**
   - the rehearsal argv carries `--baseline-hours 3`;
   - any changed end-time computation;
   - a mutation back to 2 must fail a test.
   - No fixed sleeps.
5. **Decisions file:** `docs/decisions/2026-11-28-X-rehearsal-window.md`, one short section.

**Owns:**
- `infra/fleet/steps/vm/round1-coordinator.py`;
- `docs/soak/tools/linux/test_collect_round1_coordinator.py`;
- `docs/soak/tools/linux/test_collect_round1_e2e.py`;
- `docs/soak/tools/fixtures/round1/**` (only if a fixture pins 2 h);
- the decisions file.

**Leave alone:**
- the run sheet and `docs/decisions/2026-11-27-CR-SPEC-round1.md`. The runner's CR-SPEC-R1 PR rewrites both for the 3 h window;
- `runner/`.

Ask the runner before touching `collect.py` or `analyze_cr.py`.

**Checks before `ready`:**
- `python3 -m unittest discover -s docs/soak/tools -p 'test_*.py'`;
- the linux tests under `docs/soak/tools/linux`;
- `infra/fleet` tests the way `run_tests.sh` runs them;
- follow `.claude/skills/steward/SKILL.md`.

Send `ready <PR> <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.

**Size:** small, under 150 lines. It must merge before the rehearsal (after about 2026-11-28T18:00Z), so the target is ready within 2 h of launch.
