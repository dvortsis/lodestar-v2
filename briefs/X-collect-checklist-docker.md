# X-collect-checklist-docker (Sonnet, helper): refuse not_applicable backup and auto-update answers in docker mode

Assigned by the runner on 2026-11-11 to the session that built #665 and #684. It fills the wait until CMP-9a (#681) merges; then this session takes CMP-9b.

## Why
- cr-ab-comp-1 was invalid on exactly this. The analyzer's host_checklist (CR-SPEC rule 6.5) needs `excluded` plus a next run for `backup` and `auto_update` in docker mode.
- collect.py accepts `not_applicable` there (`CHECKLIST_STATES`, collect.py:1042; the `--checklist` help at :2089).
- So a docker run can start, take hours, and then fail review.

## Do
1. In `docs/soak/tools/linux/collect.py`, at start, before anything is collected: with `--mode docker`, refuse `backup=not_applicable` and `auto_update=not_applicable`.
   - Exit non-zero with one plain line naming the item and what to pass instead (`excluded` plus its next run).
   - Native mode stays as it is.
2. Read `parse_checklist` (:1389) and the analyzer's host_checklist (`docs/soak/tools/linux/analyze_cr*.py`), so the refusal matches exactly what the analyzer requires.
3. Tests in `docs/soak/tools/linux/test_collect.py`:
   - docker with not_applicable is refused, for each item;
   - docker with excluded plus a next run is accepted;
   - native with not_applicable is still accepted.
   - Mutation-check the guard.

## Owns
- `docs/soak/tools/linux/collect.py`
- `docs/soak/tools/linux/test_collect.py`
- `docs/decisions/2026-11-11-X-collect-checklist-docker.md`

## Bar
- Run the soak tools' tests the way CI does (check `.github/workflows` for the job that runs test_collect.py).
- Branch `slice/X-collect-checklist-docker`, PR `X-collect-checklist-docker: <title>`, steward skill.
- Reply "ready" with the head when ci-ok is green.
