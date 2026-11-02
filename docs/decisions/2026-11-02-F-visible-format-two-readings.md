# F-visible-format-two-readings decisions

The follow-up that `2026-11-02-F-normalize-invisible.md` (#381) left for the runner. It applies #381's two-reading rule to the format characters that show.

- **The visible marks.**
  - The set:
    - U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2, U+110BD and U+110CD (prepended concatenation marks);
    - U+FFF9–FFFB (interlinear annotation);
    - U+13430–1343F (Egyptian hieroglyph controls);
    - U+2028 and U+2029 (line and paragraph separators).
  - On `main` they are word breaks. That lets one split a word past a term: `ba<U+0605>d` reads `ba` `d`, so a `bad` term misses. U+0605 and U+08E2 span the digits that follow them, so the split hardly shows.
  - Ignoring them alone would join words a reader sees apart, which is why #381 kept them as breaks.
- **Both readings, the stricter verdict.**
  - `normalize::visible_mark` names the set (it was `visible_format`, without U+2028/2029).
  - **The joined reading** ignores them, like the default-ignorables (`ignored()`).
  - **The earlier reading** keeps them as word breaks, exactly as `main` reads them. `mark()` is now the union of #381's `newly_ignored()` and `visible_mark()`; `Tokenizer::splitting` breaks at a mark and `splits` triggers on one.
  - Everything else is #381's machinery, unchanged: the same trigger (a name or path holding a mark, or a marked keyword entry that matches differently), each reading judged from the top, `stricter` with the earlier reading winning ties, and override, Neutralize and exception keywords matched per reading.
  - So `ba<U+0605>d` hits `bad` in the joined reading, and `alpha<U+2028>beta` still hits `beta` in the earlier one.
- **New keywords with a visible mark inside a word are refused**, by #381's check (the joined and earlier tokens differ). The message now names every kind: "the keyword has an invisible character, format mark or line break inside a word; type it without one". A pasted keyword that runs over two lines is refused with it. At the edge of a word, or next to a space, a keyword reads the same both ways and is accepted. Keywords stored before keep working through the two readings.
- **Never weaker than `main`.** The earlier reading is `main`'s reading for the same bytes and config, and the verdict that stands is never weaker than it. `tests/two_readings.rs` checks this for names, paths, keywords (marked or not), override and Neutralize entries, drop and hide lists, a user Drop rule and packs, with marks drawn from both sets.
- **Tests.**
  - The normalize unit tests check that `visible_mark` is exactly the set above and that `mark` is exactly what the joined reading ignores that `main` didn't. They also check, for each mark, that `alpha<c>beta` reads `alphabeta` joined and `alpha beta` split.
  - `the_splitting_reader_reads_as_before` includes visible marks.
  - The reference tokenizer (`tests/normalize_reference.rs`) ignores the visible set too.
  - Golden cases in `two_readings.rs` (`a_visible_mark_hits_both_ways`) cover `ba<c>d` against `bad` in a name and a path, `alpha<c>beta` against `beta` and against `alphabeta`, an override for the joined word only, and a Neutralize entry. #381's golden cases and the property test now run over both sets.
  - **Mutations**, each making a test fail:
    - the joined reading keeping visible marks as breaks (`ignored()` without `visible_mark`);
    - no second judgment for them (`mark()` without `visible_mark`);
    - the earlier reading ignoring them too.
- **Cost (§20.8).** Only names or paths holding a mark take the second judgment. Visible marks are rare in torrent names, and the ASCII fast path never looks for them. The likeliest real source is the Arabic number signs (U+0600–0605) in Arabic names; those names are judged twice. I didn't measure the budget locally (the session's disk was too small for a bench build).
  - **The bench doesn't exercise the second reading.** The §20.8 budget test (`tests/props.rs`, `budget`) judges generated ASCII release names (`torrents()`), which hold no marks, so its numbers show only the one-judgment path. A marked torrent costs about two judgments, the second with a tokenizer of its own. Adding a marked share to the bench corpus is left for the next brief that touches the budget test.
- **Stored artefacts.** Nothing that decides a verdict stores normalized tokens, as #381 recorded.
  - **Known gap, fixtures.** A false-positive example saved from now on from a name with a visible mark stores the joined tokens (`core::Fixture::of`). If the earlier reading's hit is what caught it (`alpha<U+2028>beta`, caught by `beta`), the fixtures page won't show that rule catching its own example. Fixtures only feed the counts on that page, never a verdict. Storing the earlier reading's tokens where they differ is left for the next brief that touches fixtures.
  - **For the runner and the list keeper.** The shipped term-list hashes are made in the joined reading. #381 asked for confirmation that no source term holds a format or tag character. That check should also cover U+2028/2029, which are line and paragraph separators, not format characters.
- **`Core::hits`** (previews and the review queue, tests and the fuzz target only) read only the joined reading. #386 (F-hits-two-readings) makes it read both, through the same `splits` trigger, so it covers the visible marks once both land. This PR adds a visible-mark case to its tests if #386 lands first.
- **Out of scope:** the search tokenizer (`fasttok` and `crates/search`). It still reads every Cf character as a break, the same in both readings (X-tokenizer-cf).
