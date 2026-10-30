# F-filters-packs: pack rules honour Neutralize terms (§20.5.5)
**Model:** Opus. This is content-filter semantics. Runner brief, 2026-10-30. Unblocked: F-filters (#179) and F-filters-neutral-perf (#192) have merged.

## Why
§20.5.5 is the owner's spec: a CSX-5 **Neutralize term** "stops counting as evidence in every non-floor core rule **and pack rule**", while all other evidence still counts.
- F-filters (#179) made the core rules honour Neutralize, and its decisions file (`docs/decisions/2026-10-30-F-filters.md`, "Not changed; follow-up slice F-filters-packs") records the gap.
- The pack rules **MW-3, LU-2, LU-4, FK-2 and SC-1** still read their own keyword lists without the neutral term set.

## Do
1. **Find the gap.** In `crates/filter`, find where each of those pack rules matches its keyword lists. Apply the same neutral-term handling the core rules use since #179/#192:
   - by token;
   - joined words can't borrow a neutralized word;
   - phrases use whole forms.
   Reuse the shared code; don't build a second implementation.
2. **Floor and core are untouched.** The floor and the CS-1 hash lists are never affected by Neutralize. Only pack keyword evidence changes. A neutralized word mustn't widen anything beyond its own keyword evidence, and other evidence in the same torrent still counts.
3. **Tests:**
   - unit or semantics tests per pack: a neutralized term no longer triggers that pack rule on its own, and a torrent with other pack evidence is still caught;
   - a property test that neutralizing can only remove matches whose sole evidence was that term;
   - the existing golden or intent fixtures stay green, and any changed fixture is explained.
4. **Budget:** the filter budget (§20.8, `crates/filter/tests/props.rs`, the 50 µs gate) must still pass locally. Report the measured per-case times.
5. **Decisions:** add a file naming the five packs and confirming the floor and core are unchanged.

## Own
`crates/filter/**`, filter docs and the decisions file. Nothing in `web/`.

## Done when
- fmt, workspace clippy `-D warnings`, `cargo test -p lodestar-filter` (including props) and `cargo test -p lodestar --bins` pass;
- there's a fresh Opus self-review with its blocking findings fixed;
- `/security-review`, because this is a content-safety surface.

Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally: the account is under a usage warning.
