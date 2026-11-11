# X-collect-checklist-docker decisions

`collect.py` accepted `backup=not_applicable` and `auto_update=not_applicable` in docker mode, but the analyzer's `host_checklist` (CR-SPEC rule 6.5, `analyze_cr.py`) needs each item `excluded` with a next run when the run is Docker. So a Docker run could start, take hours, and then fail review (cr-ab-comp-1). None of this loosens an owner or security decision.

- **Refused at launch.** `check_docker_checklist` runs in `Collector.prepare()` right after the checklist is parsed, before anything is collected. With `--mode docker`, either item as `not_applicable` exits non-zero with one line naming the item and saying to pass `<item>=excluded` with `--next-run <item>=<UTC time>`.
- **Native is unchanged.** Native runs still accept `not_applicable`, as the analyzer only requires `excluded` for Docker.
- **Not added:** a missing item, or `excluded` without a next run, is still caught by the analyzer rather than at launch. That fails in both modes and the brief named only `not_applicable`; a follow-up could refuse it too.
- **Tests** (`test_collect_spec.py`): docker refuses each item; docker accepts `excluded` plus a next run; native still accepts `not_applicable`. Mutating the guard (flipped mode test, wrong state, call removed) fails them.
- **Docs:** `docs/soak/tools/linux/README.md` isn't in this slice's owned paths; its example already uses `excluded`. The `--checklist` help text now says docker takes only `excluded`.
