# Brief: X-fleet-polish (Opus, helper; deps OP-3b, OP-4 merged)

Source: rv-661 review nits (reviews/rv-661-0cc76033.md N5, rv-661-f75a2109.md delta).

## Do
1. `infra/fleet/tests/test_block.py` `policy()` (about :951): it reads only `words[5]` on a query or path ACL line. HAProxy ORs further patterns on the same ACL line, so a second pattern would go unmodelled. Assert `len(words) == 6` for those ACL lines (fail the test loudly on any other shape), and add a test that a config line with a second pattern makes `policy()` fail.
2. `docs/decisions/2026-11-11-OP-3b.md:42`: "a failing start-block or arm-switch removes it at once too" holds only once Lodestar has started (before that, the proxy isn't running yet, or is removed by the block's end path). Fix the wording to say exactly when it holds. This one-line edit to the OP-3b decisions file is approved as a shared edit.
3. Decisions file `docs/decisions/2026-11-11-X-fleet-polish.md`: short, what changed and why.

## Bar
- Owns `infra/fleet/**`, plus the one line above and its own decisions file.
- No terraform init, no cloud calls, no live account IDs (only 111122223333 as a fake). Grep for 12-digit numbers before pushing.
- Run the fleet tests (`python3 -m unittest` in infra/fleet), mutation-check the new assertion (remove it: the new test must fail).
- PR titled `X-fleet-polish: <title>` from branch `slice/X-fleet-polish`. Steward skill. Reply "ready" with the head.
