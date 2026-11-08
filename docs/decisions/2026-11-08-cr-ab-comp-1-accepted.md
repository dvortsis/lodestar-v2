# cr-ab-comp-1 accepted by the owner (runner PR)

**Decided by:** the owner, 2026-11-08, relayed by the dashboard session ("I follow the runners recommendation on both items"). Recorded by the build runner. This applies CR-SPEC (`docs/decisions/2026-11-02-CR-SPEC.md`) to one segment. It does not change CR-SPEC.

- **The analyzer verdict stays as computed: `invalid (host_checklist_complete)`.** `analyze_cr.py`'s `host_checklist()` requires every checklist item to be `excluded` with a next run in Docker mode (CR-SPEC rule 5). The run recorded `backup=not_applicable`, because Appdata Backup was disabled for the run.
- **The cause was the runner's run sheet, not the run.** The CR-AB-COMP run sheet told the owner to pass `--checklist backup=not_applicable`. Backup really was off for the whole segment, and nothing else in the evidence is in doubt.
- **The owner accepted the segment** as the runner recommended (option 1 of two; the other was a fresh 25 h re-run). **The comparator result stands: 317.8 stored/min over 24 h** (bitmagnet v0.10.1, pinned by digest). The segment is cited with this file next to its verdict line, never as `valid`.
- **The run sheet is corrected:** every later run (CR-AB-ADM and on) records backup as `--checklist backup=excluded --next-run backup=<UTC>`, the same as auto-updates.
- **Follow-up X-collect-checklist-docker:** the collector accepts `not_applicable`, but the analyzer refuses it in Docker mode. The collector will refuse it at start in `--mode docker`, so a 25 h segment can't be invalidated silently again.
- **Unchanged:** no Lodestar on the host before 2026-11-09T02:01Z, then rc.2 steps B–E, then CR-AB-ADM.
