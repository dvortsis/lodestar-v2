# Dashboard adapts to work in flight (follow-up to #416 and #431)

Refines `2026-11-04-dashboard-slots.md`: the slot checks no longer treat every slice id that `runner/slices.yaml` lacks as an error. The owner asked on 2026-11-04 that the dashboard adapt by itself; the runner agreed to all three points and kept the CI consistency test strict.

- **A pull request counts for the slice its branch is named after, as well as its title id.** A follow-up PR on a reused branch (`slice/X-mem-gauges` titled `X-mem-gauges-2: …`) is in a slot if the slot names either id. "Open work in no slot" is decided per pull request, not per id.
- **A merged slice with an open pull request is not "finished work".** The slot shows it as active with the open PR.
- **A slot naming a slice that `slices.yaml` lacks is a note when a PR or a `slice/<id>` branch exists for it.** It appears as a quieter `note` row in `plan_meta` ("Slot slice not in plan files yet"), not as the "plan files are out of step" warning. With neither a PR nor a branch it stays the strict "Unknown slice" warning, because that is a real mistake.
- **An `X-*` slice with an open PR but no `slices.yaml` entry is drawn at once** as an active slice (kind `extra`) in its placement lane, or the default lane, with a "Slice not in plan files yet" note. A bare branch does not create a tree node, so stale branches add no noise. Other id families are not drawn this way.
- **CI is unchanged.** `npm test`'s consistency check still requires every slice to have a `slices.yaml` entry and a placement before a PR that touches those files merges. Only the live page is lenient.
- **No owner number or security decision changes, and no change to who edits `runner/dashboard.yaml`.**
