# X-soft-absent-void: analyze_cr voids or warns on a soft B-arm segment that lacks the soft-timeout counter

**Runner brief, 2026-11-27.** Core, Opus (the analyzer decides A/B results). Target < 400 changed lines. The plan entry already exists in `runner/slices.yaml` (runner records r17).
- **Branch:** `slice/X-soft-absent-void` from fresh `origin/main`. **PR title:** `X-soft-absent-void: analyze_cr voids or warns on a soft B-arm segment that lacks the soft-timeout counter`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.

## Why
`docs/decisions/2026-11-24-runner-records-r17.md` (X-soft-key, #1102): a soft block run on an image without #1091 runs A/A, because the switch does nothing there, yet `analyze_cr.py effect` still decides it. Nothing marks a B-arm soft segment that has no `dht_soft_timeouts_total` at all. That reverses #1102's recorded "no warning" call, a §2.19 refinement the runner flags to the owner.

## Read first
`docs/decisions/2026-11-02-CR-SPEC.md` (voids versus warnings), `docs/decisions/2026-11-24-X-soft-key.md`, `docs/soak/tools/analyze_cr.py` (`soft_timeout`, `segment`, `effect`), `docs/soak/tools/test_analyze_cr_soft.py`, `infra/fleet/tests/test_block.py`, `runner/standards.md`.

## Do
1. In a segment whose treatment is `soft` and whose arm has the switch on, a counter that is missing entirely (not zero) makes the segment void, with its own named reason. Zero over the window is a valid result, never a void. Decide void versus warning against CR-SPEC's rules and record the call and why in `docs/decisions/2026-11-27-X-soft-absent-void.md`, named as a §2.19 refinement.
2. `effect` refuses to decide a pair when either B segment is void for this reason, as it does for the other voids.
3. Tests: missing in B voids; zero in B passes; missing in A (switch off) passes; `effect` refuses the pair. Synthetic fixtures only.
4. Nit from #1102's review: `test_block.py`'s winners-length test hits the repeat rule, not the length limit. Make it test the length limit.
5. Also fix the heading of `docs/decisions/2026-11-24-X-soft-key.md` to carry "(#1102)", a one-line edit.

## Owns
`docs/soak/tools/analyze_cr.py`, `docs/soak/tools/test_analyze_cr_soft.py`, `docs/soak/tools/fixtures/**` (only new fixtures), `infra/fleet/tests/test_block.py`, `docs/decisions/2026-11-27-X-soft-absent-void.md`, and the heading line of `docs/decisions/2026-11-24-X-soft-key.md`. No crawler code.

## Bars
Run the analyzer's tests (`python3 -m pytest docs/soak/tools` or the repo's own runner) and `infra/fleet/run_tests.sh` if it covers test_block. UTC only; no model names. Push once per round of fixes; message the runner "ready <PR> <full sha>" when CI is green.
