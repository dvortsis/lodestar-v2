# F-normalize-invisible decisions (#PR)

A content-safety follow-up to `2026-11-02-X-strip-format-chars.md` (#373), which listed format characters the filter's normalization (§20.4) didn't ignore. A character that renders as nothing inside a word, in a name that reaches the filter without the feed cleaning (DHT names, imports), splits that word past a term rule while it still looks whole.

- **`invisible()` in `crates/filter/src/normalize.rs` now ignores every Cf character that renders as nothing.**
  - "Renders as nothing" is Unicode's Default_Ignorable_Code_Point, the set browsers and HarfBuzz hide. The old hand-picked list was already that set, minus a few ranges.
  - It reuses `lodestar_metainfo::format_char` (the Unicode 17.0 Cf table from #373) and excludes the Cf characters that show. A Cf character a later table adds is ignored by default, and a test that sorts every Cf character into ignored or visible names it.
  - **Newly ignored:** U+1BCA0–1BCA3 (shorthand format controls), U+1D173–1D17A (musical format controls), and the tag block U+E0000–E007F, assigned or not.
  - **Kept, unchanged:** everything it ignored before, including the non-Cf fillers U+034F, U+115F, U+1160, U+17B4, U+17B5, U+3164, U+FE00–FE0F and U+FFA0.
  - `lodestar-metainfo` was already in the filter's dependency graph (through the store and search crates), so the new direct dependency adds no crate.
- **The Cf characters that show stay word breaks, as before.**
  - These were on the #373 list: U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2, U+110BD and U+110CD (prepended concatenation marks), U+FFF9–FFFB (interlinear annotation) and U+13430–1343F (Egyptian hieroglyph controls). U+2028/2029 (line and paragraph separators, not Cf) were on it too.
  - None of them is default-ignorable. They render as a glyph, a box or a line break, so they split a word visibly, like punctuation does.
  - Ignoring them would join two words a reader sees apart. `alpha<U+2028>beta` would read as `alphabeta`, and a rule for `beta` would miss it: that would loosen matching. The fresh review found this in the first version of this PR, which followed the brief's list.
  - A test pins `alpha<c>beta` to read as `alpha beta` for each of them.
- **It only tightens.**
  - Each newly ignored character renders as nothing, so a word it splits now reads as the word a reader sees.
  - Two words with only such a character between them also read as one word. That is what a reader sees too, and someone writing a name could type the joined word directly. The old list already worked this way.
- **No stored artefact that decides a verdict holds the normalized form.**
  - User rules, keywords, overrides, exceptions and allow entries are stored as text (`filter_rules`, `override_list`, `filter_exceptions`) and normalized on each load, the same way torrent text is.
  - Every rule that parsed before still parses. Query parsing doesn't go through `invisible()`. A keyword's validation counts its words, and joining never adds words. None of the newly ignored characters is a letter or digit, so no keyword loses its words, and a one-word Neutralize keyword stays one word.
  - `filter_fixtures` keeps a core rule's hashed examples as normalized tokens (`core::Fixture::encode`), because the name itself isn't kept. An example saved from a name that held a newly ignored character keeps its old split tokens.
    - Fixtures only feed the passing and failing counts on the fixtures page (`FiltersApi::fixtures`), never a verdict. At worst one example is counted wrongly.
    - They can't be worked out again without the name, and this slice doesn't own the store, so they're left as they are.
  - `filter.stand_ins` and the unreadable-row list are keyed by rule id, and the engine's version is a counter. Neither depends on tokens.
  - The shipped core and pack term lists are hashes of tokens (`examples/hash_terms.rs`).
    - A source term that held one of the newly ignored characters would have been hashed as split words.
    - The plain lists are kept out of the repository, so this slice can't check them.
    - The runner should have the list keeper confirm that no source term holds a format or tag character.
- **The reference tokenizer test** (`tests/normalize_reference.rs`) writes the same set out on its own, so it doesn't share the code under test's table.
  - Its generator now includes newly ignored and visible characters, so the test still checks that the planned tokenizer gives exactly the reference's tokens.
  - `format_char` itself is checked against `unicode-properties`, but a Cf character added in a newer Unicode only shows up once that dev-dependency is bumped.
- **Tests:**
  - One walks every code point. It checks that `invisible()` is exactly the set written out in the test, that every Cf character is either ignored or a known visible one, and that `hel<c>lo` gives the tokens of `hello` for every ignored character.
  - Golden cases cover both ends of each newly ignored range, and each visible range.
  - A property test inserts any ignored character anywhere in a text. The tokens never change, and neither does whether a keyword matches.
- **Out of scope: the search tokenizer.** `crates/search/src/tokenizer.rs` treats every Cf character as a word break, U+200B and U+00AD included.
  - User query rules read through it (`fasttok`), so their words, phrases and NEAR windows still split on any of them.
  - Only the core, pack terms and keyword entries read through `normalize`.
  - Changing it is the separate follow-up X-tokenizer-cf, which may need an index rebuild. That follow-up should drop only the default-ignorable characters, as this one does.
