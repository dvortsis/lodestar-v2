# Brief: X-soak-screen-calibrate (Opus; testing priority)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-soak-screen-calibrate` from `origin/main` (after #777, c904f883). **PR title:** `X-soak-screen-calibrate: calibrate the soak screen against replayed runs`.

**Why now:** testing comes first (owner §2.62). The soak screen in `analyze_cr0.py` is advisory, but it misreads both known runs:
- on the **passing** r5 it trips: `mimalloc_committed_mib_per_1000_stored` is 37.79 against a threshold of 20;
- on the **failing** r4 it says `not_available`, because `mmap_cache_excess[main]` is absent.

A screen that trips on a pass and can't see a fail tells the owner nothing.

## Read first
- `docs/soak/tools/analyze_cr0.py`: the screen section (search `screen`, `mimalloc_committed`, `mmap_cache_excess`) and `MEMORY_FIT`. #777 (M8-WIN-1) just added `windows_checks`; leave it alone.
- `docs/soak/tools/test_analyze_cr0.py`, `docs/decisions/` for X-soak-screen (#562) and X-soak-screen-fixes (#595), and `.github/workflows/soak-screen.yml`.
- `docs/decisions/2026-10-30-CR-0-acceptance.md` (the gate; it doesn't change).

## What to build
1. **The mimalloc growth input:** decide from the replayed evidence whether to recalibrate it (a threshold with a stated basis) or demote it to information only. Show the numbers for r4 and r5 in the decisions file: aggregates only, no raw logs, hashes or paths from the owner's runs.
2. **Absent inputs:** when one input is missing (for example `mmap_cache_excess[main]` on a run whose collector predates it), the screen still decides on the inputs it has and names the missing one. It says `not_available` only when nothing usable is present.
3. **The outcome to aim for:** r5 → no trip, r4 → trip, from synthetic fixtures that copy their shape. **Never commit owner soak data.** Build fixtures by hand with invented values.
4. **The screen stays advisory.** `MEMORY_FIT`, its numbers and the memory gate don't change, and an existing test asserts them. Keep it green.
5. **Tests** in `test_analyze_cr0.py`: an r5-shaped pass, an r4-shaped trip, a missing-input case for each input, and every-input-missing → `not_available`. Standard library only.
6. **Decisions file** `docs/decisions/2026-11-12-X-soak-screen-calibrate.md`: what changed, the basis for each threshold, and that the screen is still advisory. This is a runner call (§2.26/§2.33), flagged to the owner per §2.19.

## Owned paths
`docs/soak/tools/analyze_cr0.py` (the screen only), `docs/soak/tools/test_analyze_cr0.py`, the decisions file. Ask the runner before any other file. Don't touch the collectors, `soak-screen.yml`, `MEMORY_FIT` or `windows_checks`.

## Checks before the push
- `python3 -m unittest discover -s docs/soak/tools -p 'test_*.py' -v`
- Re-read the diff: no owner data, no 12-digit numbers, no local times (UTC only).

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. When ci-ok is green, message the runner "ready <pr> <full 40-char sha>".
