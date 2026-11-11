# X-collect-checklist-launch-2 decisions

X-collect-checklist-docker (#688) refused `not_applicable` in docker mode but left the rest of rule 6.5's host checklist to the analyzer. This closes that gap. None of it loosens an owner or security decision.

- **What the analyzer requires.** `analyze_cr.py`'s `host_checklist` fails `host_checklist_complete` when an item is absent, or is `excluded` with no `next_run_utc`, in **both** modes; only `not_applicable` is docker-specific (refused by #688). So native needs the same two checks, and there was no reason to limit them to docker.
- **Refused at launch, in both modes.** `check_checklist_complete` runs in `prepare_spec()` right after the checklist is parsed (the checklist is only recorded and analyzed for CR-SPEC run kinds). It refuses an item that isn't stated, and an `excluded` item without `--next-run`, each with one line naming the item and the fix. The missing-item line offers `not_applicable` only outside docker mode.
- **Order.** The docker `not_applicable` refusal still comes first, so a Docker run that passes `not_applicable` keeps #688's message.
- **Tests** (`test_collect_spec.py`): docker refuses a missing item and an excluded item without a next run; native refuses the same and still accepts `not_applicable` and a stamped `excluded`. Mutating the guard (missing check, next-run check, the call, the native hint) fails them.
