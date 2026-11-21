# X-filter-dicp-doc decisions

Runner brief X-filter-dicp-doc, 2026-11-21. The filter's `invisible` (`crates/filter/src/normalize.rs`) is documented as Unicode's Default_Ignorable_Code_Point set, and `invisible_is_exactly_the_default_ignorables` claims to write that set out. Both left out the Mongolian free variation selectors U+180B–180D (FVS1–3) and U+180F (FVS4); U+180E, the vowel separator, was in. `crates/lodestar/src/windows/acl.rs`'s written-out set (X-win-path-ignorables) has them, so the two tables disagreed.

## Decision: list them in `invisible`, not in `newly_ignored`

This is the brief's option (a) without its `newly_ignored` step, because the code shows (b)'s condition: these characters are already folded away before `invisible` is read.

- **They're combining marks.** All four are category Mn (unlike U+180E, which is Cf), and `unicode_normalization`'s `is_combining_mark` holds for each.
- **Combining marks are dropped first.** `plan_char`, which makes every non-ASCII character's reading, skips a combining mark before it asks `ignored`, and through it `invisible`. That is the only place either is read for matching. The matching before F-normalize-invisible dropped them the same way, and so does `tests/normalize_reference.rs`'s reference tokenizer.
- **So listing them changes no reading.** They're dropped in both readings, as before. A text holding one reads as it did, and nothing that matched before stops matching. The filter neither loosens nor tightens. What changes is that `invisible` is now exactly the Unicode set, as its doc says, and agrees with `acl`'s.
- **Why not `newly_ignored` too:**
  - That set is what the matching before read as word breaks, and the earlier reading never broke on these: it dropped them as marks.
  - Listing them there would mark a text holding one for a second reading (`splits`, `mark`). That reading would come out the same, because `plan_char` still drops them first, so it would only spend the filter budget.
  - The exhaustive check already leaves combining marks out of its `newly_ignored` comparison, for the same reason (the variation selectors U+E0100–E01EF). The doc comment now names the Mongolian ones beside them.

## Tests

- **`invisible_is_exactly_the_default_ignorables`:** the expected set now has U+180B–180F. It still checks every code point:
  - `invisible` against Default_Ignorable_Code_Point;
  - `newly_ignored` and `mark` against what the matching before read as breaks;
  - every Cf ignored or a known visible mark.
- **`mongolian_free_variation_selectors_read_as_nothing`** (new), for each of the four:
  - it's a combining mark and `invisible`, and not `newly_ignored` or a `mark`;
  - its `plan_char` reading is empty;
  - `hel·lo` reads as `hello` and never as `hel`;
  - `alpha·beta` reads as `alphabeta`;
  - the rule `alpha beta` still matches `The.Al·pha.·Be·ta·.2019`;
  - a text holding one asks for no second reading.

  U+180E stays an ignored format character.

- **`tests/normalize_reference.rs`:** the reference's own `invisible` has the same range. Its tokenizer drops combining marks first too, so its readings don't change.
- **Mutation checks:**
  - Dropping the range from `invisible` fails `invisible_is_exactly_the_default_ignorables` and the new test.
  - Adding the four to `newly_ignored` fails the new test.
- **The §20.8 filter budget:** unchanged, with 50 µs and the same cases. The change adds one range to a table read only when a character's reading is first planned, which is then cached.
  - Five local release runs give medians within this container's noise of `main`'s (X-filter-budget-remeasure's A/B on the same machine): the mixed set at commit 55.7 µs against 54.9, the calibration 11.0 µs against 10.8.
  - This unlisted CPU fails the gate on `main` too, so CI's `filter budget` job, which runs on this PR, decides.

## Owns

`crates/filter/src/normalize.rs`, `crates/filter/tests/normalize_reference.rs`, this file, this slice's entry in `runner/slices.yaml`, and its placement line in `runner/dashboard.yaml`.
