# X-filter-dicp-doc

Runner brief, 2026-11-21Z. Slice id `X-filter-dicp-doc`, branch `slice/X-filter-dicp-doc`, PR title `X-filter-dicp-doc: <title>`.

## Why
`crates/filter/src/normalize.rs` `invisible` is documented as Unicode's Default_Ignorable_Code_Point set, and the test `invisible_is_exactly_the_default_ignorables` claims to write that set out. Both omit the Mongolian free variation selectors U+180B–180D and U+180F (FVS1–3 and FVS4; category Mn, default-ignorable since Unicode 14). U+180E is already in. `crates/api`'s `acl::default_ignorable` (merged in X-win-path-ignorables) does include them, so the two tables disagree.

## What
1. Decide from the code which of these is right, and say which in the decisions file:
   - **(a), preferred:** add U+180B–180D and U+180F to `invisible`. A character the matching before did not ignore must also go into `newly_ignored`, so that text holding one is read both ways and nothing that matched before stops matching (`docs/decisions/2026-11-02-F-normalize-invisible.md`). This only tightens the filter.
   - **(b):** if (a) could make any term stop matching, or the code shows they're already folded away before `invisible` runs, keep the behaviour and fix the doc comment and the test so they state the omission and the reason.
2. Bring the test's expected set, `tests/normalize_reference.rs`'s `invisible` and `newly_ignored_ranges_join_a_word` (if (a)) in line, with an exhaustive check over the whole code space where one already exists.
3. Show that the §20.8 filter budget test still passes locally (it now re-measures once, from #1009). Its 50 µs and its cases stay unchanged.
4. Add a decisions file `docs/decisions/2026-11-21-X-filter-dicp-doc.md`, and this slice's own `runner/slices.yaml` entry if no placement rule covers it.

## Owns
- `crates/filter/src/normalize.rs`
- `crates/filter/tests/normalize_reference.rs`
- a new test file under `crates/filter/tests/` if needed
- the decisions file
- this slice's own `slices.yaml` entry

Send the runner the list before you edit anything else.

## Bars
`runner/standards.md`. This is content-safety code: the review includes /security-review, and nothing may loosen what the filter blocks. Run the Rust gates before pushing. No fixed sleeps. Push once per round of fixes, and send "ready <pr> <sha>" when CI is green.
