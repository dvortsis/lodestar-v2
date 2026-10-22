# Q-docs: the query language reference
**Model:** Sonnet (docs). **Branch:** `slice/Q-docs`.

**Spec:**
- architecture §8 (the query language block and the ranking notes) and §16 (every query-language decision: M2/M3, B5a cursors, B17 adult visibility, the stem fallback);
- the parser and compiler in `crates/search/src/{query.rs,compile.rs,tokenizer.rs,stem.rs}` (read only);
- the golden and property tests that show real behaviour;
- `docs/ui-glossary.md` (wording);
- §10.14 (UX writing).

## Own
- `docs/query-language.md` (new);
- one link line in `README.md` (or `docs/architecture.md` §8 if the README has no docs list);
- a doc-test-style example file, `docs/query-language-examples.txt`, only if you add a test that parses every example. You may add `crates/search/tests/doc_examples.rs` for that, and it must pass.

## Write
A user-facing reference, accurate to the code, not the plan. Where the plan and the code differ, the code wins; list each difference in the PR.
- Every field (name, aliases, value syntax, units, ranges, relative dates, examples), operators (AND/OR/NOT/`-`/`!`, grouping, phrases, NEAR, `file:(…)`, regex), and the hash/magnet paste.
- How ranking and the plurals fallback work, in plain words.
- Counts ("≈"), cursors, and why results can say "reload".
- Errors (unknown fields give a helpful error), Adult visibility, and hidden torrents.
- Every example must parse. Add `doc_examples.rs` to prove it: it parses each example with the real parser and fails on error.

## Done when
- After merging `origin/main`: `cargo test -p lodestar-search` passes (if you added the test), and `npm run check` in `web/` is unaffected.
- The PR lists every plan-vs-code difference found.
