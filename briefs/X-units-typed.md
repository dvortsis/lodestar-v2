# X-units-typed (runner brief, 2026-11-22Z)

Track U, from UI review U12 ("accept GB as typed", accepted under owner-decisions §2.69 item 17.6). Kind: core (the query-language parser, which also reads stored filter rules). Opus throughout. /security-review before "ready". Deps merged: X-search01-fix, X-ui-plan-r5.

## The rule (already decided; don't re-decide)
§10.12 as #996 wrote it, and `docs/decisions/2026-11-20-X-ui-plan-r5.md` "The rule behind U12":
- **Two readings.** The stored-text reading (KB/MB/GB/TB 1,024-based) stays the parser's default, so `crates/filter` and every stored caller keep it, with no change and no migration.
- **Only these opt in** to the typed-unit reading (GB = 10^9, GiB = 2^30): the query bar, typed facet values, natural phrasing, and `/search?q=`. They opt in at `request_ast` in `crates/api/src/search.rs`, so facets, export, live search and saved-search previews read the same set as search.
- **The reading covers `size:` and `largest:`.** `piece:` stays 1,024-based.
- **The printer writes only bytes or IEC units,** so stored query text is always canonical and means the same under both readings.
- **The chip echoes the user's typed text** in `clauses.ts`, without `formatBytes`.

## Step 0 (send me, then wait for "go")
1. **Every path that parses a query,** with file:line: which reading each gets after the change, and why. Include saved searches and alerts (stored), filter rules (stored), the export, live search, facets, `/suggest`, natural phrasing and the web chip.
2. **The proof that stored text can't change meaning.** A rule saved after the change matches the same set under the previous parser, and a stored `size:<1GB` still matches under 1 GiB.
3. **The parser entry point:** how the reading is passed (an enum or option on the parse call, never a global), and that the default is the stored reading.
4. **Fuzz and property tests:** the existing `query` fuzz target covers both readings, and property tests cover the size grammar for both.
5. Files and an estimate.

## Rules
- No change to `crates/filter`, the stored format, or any stored caller's reading.
- No new `unsafe`. No fixed sleeps.
- Never beside RX-1 or ENR-11 (query.rs), X-zero-rescue (clauses.ts) or X-alert-noun (the search messages). None of them is running now.
- Bench: the parse and search budgets (§3) must not regress. Run the search bench if `bench/` covers parsing, and give the numbers.

## Owns
- `crates/search/src/query.rs`
- `crates/search/src/natural.rs`
- `crates/api/src/search.rs`
- `web/src/lib/features/search/query/clauses.ts`
- `docs/query-language.md`
- `docs/decisions/2026-11-22-X-units-typed.md`
- Tests next to those files, and `fuzz/fuzz_targets/query.rs` if the target needs the reading. Ask before anything else.

## Branch and PR
- Branch `slice/X-units-typed`. PR title "X-units-typed: Query language and natural phrasing accept GB and MB as typed and echo them back".
- Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-search -p lodestar-api -p lodestar-filter`, the query fuzz target briefly (`cargo +nightly fuzz run query -- -max_total_time=120` if nightly is installed), and in web/ `npm run check`, `npx vitest run` and `npm run size:ci`.
- Run /security-review on the branch before "ready", and put its result in the PR description.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
