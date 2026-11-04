# SRC-8b: the RSS parser reads a bare `<infohash>` element (Academic Torrents)
**Model:** Opus. Untrusted-input parser, `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
Academic Torrents' feed (`https://academictorrents.com/rss.xml`) carries each item's hash as a plain `<infohash>` element with no namespace. Its guid and link are `/details/<hash>` page URLs, and it has no magnet. SRC-8's `crates/sources-feeds/src/rss.rs` reads hashes only from `nyaa:infoHash` (matched by namespace URI) or from a magnet, so every item is skipped as `no_hash`. SRC-10d (the plugin) waits on this slice.

## Do
1. In `rss.rs`, read a no-namespace `<infohash>` child of `<item>` (RSS 2.0) and of `<entry>` (Atom, only if that's trivially symmetric), through the **existing** `hash()` validation:
   - zero hashes are rejected;
   - an invalid value counts as `bad_hash`;
   - the existing length and caps apply.
2. **Precedence:** keep the existing sources first. If more than one is present and they disagree, follow the parser's existing conflict rule, or treat it as `bad_hash` if there's none. Document the choice.
3. **No new unbounded state.** All the existing caps (depth, items, text length, total bytes) still apply.
4. **Tests:**
   - a scrubbed recording of real Academic Torrents items in the parser's fixtures (record it through the proxy; the host is allowed);
   - golden tests for valid, uppercase, wrong-length, zero and duplicate-conflicting hashes;
   - extend SRC-8's existing fuzz target and property tests to cover the new element.
5. **Decisions file:** `docs/decisions/2026-11-04-SRC-8b.md`.

## Own
`crates/sources-feeds/src/rss.rs`, `crates/sources-feeds/tests/rss*.rs`, the RSS fixtures, `fuzz/fuzz_targets/rss.rs`, and your decisions file.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the fuzz target builds, and `/security-review` and a fresh self-review are done.

Branch `slice/SRC-8b`. The PR is titled "SRC-8b: RSS parser reads a bare infohash element". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
