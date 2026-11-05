# Dashboard shows free slots while work is ready

Owner, 2026-11-05 (dashboard session): keep the slots full while there is work, until the planned cut to four slots at 2026-11-06T00:00Z.

- **`plan_meta` gains an `idle` row, "Free slots, work ready".** It appears only when fewer sessions run than `limits.phase_total` and at least one slice is ready: no unmet dependency, not an epic, not running (listed in a slot or with an open PR), and its `owns` do not overlap a running slice's. The note lists up to six, most-unlocking first.
- **It is not a warning.** It never counts toward the "plan files are out of step" header, because nothing in the plan files is wrong. The page shows it on the quiet line under the header.
- **Overlap is conservative.** A glob is compared by its prefix up to the first `*`; a slice with no `owns` is never blocked, and an `owns` entry that starts with `*` is treated as overlapping everything.
- **The runner still decides what to launch** (`docs/owner-decisions.md` §2.36). The row is a prompt to look, and a slot may stay empty for a reason the runner records.
- **No owner number or security decision changes.**
