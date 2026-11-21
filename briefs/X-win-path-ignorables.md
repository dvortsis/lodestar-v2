# X-win-path-ignorables

Runner brief (2026-11-21). Follow-up to #1004 (X-win-log-path-display), from its fresh review's optional findings. Branch `slice/X-win-path-ignorables`; PR title `X-win-path-ignorables: escape every default-ignorable in Windows path display`.

## Why
`quoted()` in `crates/lodestar/src/windows/service.rs` escapes control characters, `"` and `acl::invisible`. But `acl::invisible` (`crates/lodestar/src/windows/acl.rs`) covers only Cf plus Zl/Zp. Some characters draw nothing and still pass through raw: variation selectors (U+FE00–FE0F, U+E0100–E01EF), the combining grapheme joiner U+034F, the Hangul fillers (U+115F, U+1160, U+3164, U+FFA0), the Khmer inherent vowels U+17B4–17B5, and the Mongolian free variation selectors U+180B–180F. The filter crate already has the exact Default_Ignorable_Code_Point set, with an exhaustive test (`crates/filter/src/normalize.rs`, `invisible`).

## Do
1. **One set, not two.** Make `acl::invisible` true for every Default_Ignorable_Code_Point, and keep what it covers today: all of Cf, including the visible format marks (escaping those in a log is harmless), and Zl/Zp.
   - Prefer reusing the filter crate's function: make it `pub` (or add a `pub fn default_ignorable`), and call it from `acl::invisible` alongside the existing checks.
   - Don't change the filter crate's behaviour; its tests must stay green.
2. **A test that pins the set.** Every code point that filter's set calls invisible is escaped by `quoted()`, and so are U+2028/U+2029. Ordinary letters, digits, CJK and combining marks are left alone, apart from the existing rules. Walk all of `char` (as normalize.rs's test does) or a well-chosen table.
3. **Catch the Linux-blind sites.** In the service.rs tests, three call sites differ from `{:?}` only on Windows: the config-file path at about :309 and :336, and "is a file" at about :575. Name the test's config file and its in-the-way file with a literal backslash or a TAB (both legal in Linux file names), so that reverting any of them to `{:?}` fails on ubuntu-latest too.
4. **A test for the remaining site.** The `"{}: {e}"` site at about :589 (a metadata error other than NotFound on a parent folder) has no test. Add one if it can be triggered without root or fixed sleeps (for example a parent that isn't a directory, or a permission-denied parent run as non-root with a skip-if-root check that doesn't mark the test ignored). If it can't be triggered portably, say so in the decisions file.
5. **`acl::secure_dir` messages.** At acl.rs about :485, the "creating …" and "reading …'s permissions" errors show the folder with plain `display()`, and those reach the temp log unchanged. Route them through the same quoting, moving `quoted()` to wherever both files can reach it.
6. **Decisions file wording.** Update #1004's decisions file, last bullet (about line 26), so it says which reverts only windows-latest catches, if any remain after step 3. Add your own decisions file, `docs/decisions/2026-11-21-X-win-path-ignorables.md`.

## Owns
- `crates/lodestar/src/windows/acl.rs`, `crates/lodestar/src/windows/service.rs`
- `crates/filter/src/normalize.rs` (visibility only, plus a doc line), `crates/filter/src/lib.rs` (only if a re-export is needed)
- #1004's decisions file (the one bullet) and the new decisions file
- `runner/slices.yaml` (your entry), `runner/dashboard.yaml` (your placement line, only if no rule covers `X-`)

## Not
- No change to the Event Log line (`one_line`, its 260 cut, no quotes).
- No new dependency, no `unsafe`, no fixed sleeps.
- Don't touch `crates/lodestar/src/settings/preferences.rs`'s own `invisible`; if you think it should share the set too, say so in the PR, and the runner will brief it.

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar -p lodestar-filter`, `cd tools/status-connector && npm test`. Mutate `quoted()`'s ignorable check and each call site you touch, and record the table in the decisions file.

When CI is green on your head, send the runner `ready <pr> <sha>`.
