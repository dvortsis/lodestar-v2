# X-damaged-mark-links (Opus, fleet infra; follow-up to #849)

The runner decided this slice; it's the follow-up to #849's review.

## Why
copy-off now treats a `damaged` mark that is a dangling symlink as a mark (`-e || -L`). The host-side checks still test `-e` only, so they disagree with copy-off: a dangling link counts as a mark in one place and no mark in the others. Also, `guard_lost`'s write of the mark would follow a dangling link.

## Do
1. `infra/fleet/steps/vm/vm-status.sh` (`block_intact`), `outage-watcher.sh` and `block-lib.sh` (the mark test near :133): treat the mark as present when `-e || -L`, the same as copy-off.
2. `block-lib.sh` (the mark write near :135): never write through a symlink. If the mark is already a link, leave it in place; it already counts as a mark. Otherwise create the file without following a link, for example with `set -o noclobber`, or by testing `-L` first.
3. Wording nits from #849's review:
   - `scrub.py:179` and the `copy-off.sh` comment: the stand-in is `/dev/null` (a character device), not "an empty file".
   - In `docs/decisions/2026-11-13-X-scrub-guard-fields.md`, reword "copy-off mounts the mark" and "The mark is mounted" to "a stand-in for the mark is mounted".
4. Tests in `infra/fleet/tests/test_block.py` (or `test_steps.py`, whichever already covers each script):
   - a dangling-link mark counts as damaged in each host-side check;
   - guard_lost never writes through a link (to a file, or dangling);
   - the symlink test from #849 also asserts that `scrub.collect` returns `damaged: True` for the stand-in;
   - drop the no-op `assertNotIn(str(self.tmp / "missing"))`.

## Owns
`infra/fleet/steps/vm/{vm-status.sh,outage-watcher.sh,block-lib.sh,copy-off.sh,scrub.py}`, `infra/fleet/tests/test_block.py`, `infra/fleet/tests/test_steps.py`, a line in that decisions file, and your own placement line in `runner/dashboard.yaml`.

## Rules
- No cloud calls, no terraform.
- No fixed sleeps.
- Keep it small (≲ 150 lines).
- Follow the steward skill, and send "ready <PR> <sha>" to the runner.
