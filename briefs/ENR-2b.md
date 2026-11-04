# ENR-2b: ThePornDB response parser on json.rs, bounded and fuzzed
**Model:** Opus, with `/security-review`: this parses untrusted input. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33.

## Spec
- **The plan:** `docs/plan/2026-11-04-v06-rulepacks.md`: the ENR-2b / ENR-3 bullet (around line 157), "New dependencies" (around line 79: none, and no `serde_json`), and shared files (around lines 115-122).
- **Architecture:** §18.9 (bounded parsers) and §20.9 (enrichment).
- **The reader:** `crates/sources-feeds/src/json.rs`, ENR-2a, merged as #422. Use it read-only.
- **House style:** `crates/sources-feeds/src/common.rs`, `archive_org.rs` and SRC-9's tests.

## Do
1. **A pure function** from ThePornDB's REST JSON responses (scene/movie search and lookup by hash or title) bytes to matches plus skip counts. No I/O.
   - Caps on total bytes, depth, items and string lengths.
   - Malformed items are skipped and counted, never half-used.
2. **Extract only the fields ENR-0 can store:** the match's kind (scene, movie, VR where the service has it), the service id, the title, and performers as name and id. Whether performers are kept is decided later (Q4), so return them separately and make them easy to drop.
3. **Never request or keep** dates of birth, or any other personal field such as measurements, birthplace, aliases' personal data or images of people (plan Q5). A test asserts that a response carrying such fields yields none of them.
4. **Findings from #422's review, for you:**
   - `json`'s `Stop` doesn't latch: after an `Err` the cursor sits mid-token. **Always propagate `Err` with `?`, never swallow it.** Add a test that a mid-value error stops the parse.
   - Booleans and nulls are read as `peek` then `skip`.
5. **Fixtures:**
   - Recording from the live service needs an account, an API key and network access this environment doesn't have, so build fixtures from the service's public API documentation and schema examples.
   - Mark them "synthetic, from public docs" in a fixture README.
   - Recording real fixtures, with keys scrubbed, moves to the plugin slice (ENR-4 / ENR-5). Say so in the decisions file.
   - Use obviously synthetic names and ids, never real performers.
6. **Tests:**
   - property tests: every cap stops exactly; arbitrary input never panics; a skipped item never leaks partial fields;
   - golden tests on the fixtures.
7. **A fuzz target** `fuzz/fuzz_targets/tpdb.rs`, run 10 minutes clean locally and reported in the PR. Seed it from your fixtures (`fuzz/seeds/tpdb/`).

## Own
- `crates/sources-feeds/src/tpdb.rs`, `crates/sources-feeds/tests/tpdb.rs`, `crates/sources-feeds/fixtures/tpdb/**` (or wherever the crate keeps fixtures; name it), `fuzz/fuzz_targets/tpdb.rs` and `fuzz/seeds/tpdb/**`.
- The decisions file `docs/decisions/2026-11-04-ENR-2b.md`.
- **Shared edits, named in the PR:** one `pub mod tpdb;` line in `crates/sources-feeds/src/lib.rs`, and one `[[bin]]` in `fuzz/Cargo.toml`. ENR-2b and ENR-3 run in parallel and both add these lines; whichever merges later brings `main` in.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the fuzz target ran 10 minutes clean, `/security-review` is clean, and a fresh self-review is done.

Branch `slice/ENR-2b`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round. Times in docs are UTC (§2.35).
