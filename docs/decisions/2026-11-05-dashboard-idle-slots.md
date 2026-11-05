# Dashboard shows free slots while work is ready (#452)

Owner, 2026-11-05 (dashboard session): keep the slots full while there is work, until the planned cut to four slots at 2026-11-06T00:00Z.

- **`plan_meta` gains an `idle` row, "Free slots, work ready".** It appears only when fewer sessions run than `limits.phase_total` and at least one slice is ready: no unmet dependency, not an epic, not running (listed in a slot or with an open PR), and its `owns` do not overlap a running slice's. The note lists up to six, most-unlocking first.
- **It is not a warning.** It never counts toward the "plan files are out of step" header, because nothing in the plan files is wrong. The page shows it on the quiet line under the header.
- **Overlap is conservative.** A path is cut at its first `*`, `?`, `[` or `{`. If either side was cut, the two overlap when one cut prefix starts with the other, with no `/` boundary (`auth*.rs` against `auth_session.rs`, `{a,b}.rs` against `a.rs`, a pattern starting with `*` against anything). SvelteKit's literal `[stage]` directories therefore count as possible overlaps, which is the safe direction. Two plain paths overlap when equal or one contains the other. A slice with `owns: []` is never blocked and is offered vacuously.
- **The row says nothing when the slot list cannot be trusted:** malformed, undated, future-dated or stale (over 24 h), or an unknown limit. It adds an `ids` field with the full candidate list (up to 64).
- **A non-list `owns` is a warning ("Bad owns")** and counts as owning nothing.
- **The runner still decides what to launch** (`docs/owner-decisions.md` §2.36). The row is a prompt to look, and a slot may stay empty for a reason the runner records.
- **No owner number or security decision changes.**
