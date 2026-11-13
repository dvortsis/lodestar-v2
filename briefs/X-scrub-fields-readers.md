# X-scrub-fields-readers (Opus, fleet infra; follow-up to #848 and #851)

The runner decided this slice.

## Why
#848 added the `host_guard_drops` (count) and `damaged` (boolean) fields to the scrubbed evidence. Nothing reads them yet, so a damaged block or a guard drop can't show up in any summary or verdict.

## Do
1. Find every place that consumes the scrubbed evidence document: the control plane's evidence summary and result comment, and the analyzer or report code under `infra/fleet/` (and `docs/soak/tools/` if it reads the same document). Use `git grep` on the existing field names, e.g. `outages`.
2. Surface both fields wherever the other block-health fields are shown:
   - `damaged: true` makes the block's result not clean. Use the same severity and wording pattern as the existing failures, and state that it's the reason.
   - `host_guard_drops > 0` is reported as a count.
   - A document that predates #848, with neither field, reads as "not reported", never as clean or damaged.
3. **Nit 1 from #851's review.** `docs/decisions/2026-11-13-X-scrub-guard-fields.md` (~:37) overclaims: noclobber only refuses links to regular files and dangling links. Either narrow the sentence to "a link to a file, or to nothing", or make guard_lost's mark creation race-free (write a temp file in `$BLOCK`, then `ln -T` it into place, which never follows a link). Prefer race-free if it stays small. If you choose it, add a test that exercises it.
4. **Nit 2.** `test_guard_lost_writes_a_new_mark_once` (test_block.py ~:1072) runs guard_lost once. Run it twice and assert one `host_guard_lost` line and an unchanged mark, or rename the test.
5. **Rollout note** in the decisions file: blocks already running keep the guard copy installed at start until they're restarted.

## Owns
- The evidence readers you find under `infra/fleet/`, and their tests.
- `infra/fleet/steps/vm/block-lib.sh` (guard_lost only) and `infra/fleet/tests/test_block.py`.
- That decisions file.
- Your own placement line in `runner/dashboard.yaml`.

If a reader lives outside `infra/fleet/`, ask the runner before editing it.

## Rules
- No cloud calls and no terraform.
- No fixed sleeps.
- Keep it ≲ 300 lines.
- Follow the steward skill, and send "ready <PR> <sha>".
