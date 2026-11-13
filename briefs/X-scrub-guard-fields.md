# X-scrub-guard-fields (Opus; content-safety surface)

Runner brief, 2026-11-13Z. Branch `slice/X-scrub-guard-fields` from current main. PR title `X-scrub-guard-fields: <short title>`.

## Why
#845 (X-gluetun-guard-reboot) records host-guard drops and a `damaged` mark in the block directory, but the copy-off scrubber
(`infra/fleet/steps/vm/scrub.py`, an allow-list) doesn't carry them, so the evidence that comes back can't show a block whose guard dropped
or that was marked damaged. Follow-up N7 from #845's review.

## Do
1. In `scrub.py`'s `collect()` and its allow-list spec, add:
   - `host_guard_drops`: a count (`num`), read the same way `outages`/`outage_notes` are counted (reuse the existing block-lib source of the count; don't parse free text).
   - `damaged`: a boolean (present or absent), never the file's contents. The mark's text is not evidence and must not pass.
   Both must go through `clean()` with their shape check, and be dropped by `scrub(doc)` in the control plane if malformed.
2. Older blocks without these files still scrub (fields absent or 0/false, your call, stated in the decisions file).
3. Tests in `infra/fleet/tests/` (the existing scrub tests' file): a block with drops and a damaged mark; one without; a damaged file containing hostile text (an IP, a hex ID, a path) proves only the boolean survives; a malformed count is dropped.
4. If the analyzer or the runbook reads the scrubbed fields, name the gap as a follow-up; do not edit them unless in your owns.
5. Decisions file `docs/decisions/2026-11-13-X-scrub-guard-fields.md`.

## Owns
`infra/fleet/steps/vm/scrub.py`, the scrub tests in `infra/fleet/tests/` (add cases; don't restructure), the decisions file, and your one placement line in `runner/dashboard.yaml`.

## Rules
Allow-list stays an allow-list: no free text, no new shapes broader than the existing ones. No cloud or AWS calls. No IPs, instance IDs or 12-digit numbers in tests beyond obviously fake documentation values. Run `python3 -m unittest` for the fleet tests and the infra CI job's local equivalent. Follow `.claude/skills/steward/SKILL.md`. Push once per round; reply `ready <PR> <full 40-char sha>`.
