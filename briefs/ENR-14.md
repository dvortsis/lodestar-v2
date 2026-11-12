# ENR-14: Wikidata parser and plugin, the pattern for v0.7 enrichment plugins

**Runner brief, 2026-11-12 (UTC).** Opus, core work (it parses untrusted input), `/security-review`.
- **Branch:** `slice/ENR-14` from fresh `origin/main`, at or after 38827fd5.
- **PR title:** `ENR-14: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
Eight enrichment plugins (tmdb, tvdb, anilist, mb, ol, igdb, discogs, steam) wait on this one, and four more after owner answers. Wikidata is the **pattern**: whatever shape it sets — parser, caps, fixtures, fuzz target, plugin registration, tests — the others copy. Make it plain, small and easy to copy. The plan estimates about 800 lines.

## Read first
- `docs/plan/2026-11-05-v07-m7.md`: the ENR-14 row, the "may land before the cut, inactive" rule, and §6 Q5 and Q12 (the owner's decisions).
- `docs/decisions/2026-11-12-ENR-10.md`: the harness, `IdentifySource`, `ResolvedVia`, eligibility, the cache, the budget, and its notes for later plugins.
- `docs/decisions/2026-11-06-ENR-9.md` and `-ENR-9b.md`: the v3 claims, and how every details text is judged before storage.
- `docs/decisions/2026-11-07-runner-followups.md`: the ingest judged-bytes budget.
- `docs/architecture.md` §18.1, §18.2, §18.7 and §18.9; CLAUDE.md; `runner/standards.md`.
- An existing feeds parser and plugin pair for the house style, for example `crates/sources-feeds/src/tpdb.rs` with `crates/sources-plugins/src/tpdb.rs` and their fuzz target.

## Owns
- `crates/sources-feeds/src/wikidata.rs`, `crates/sources-feeds/tests/wikidata.rs`
- `fuzz/fuzz_targets/wikidata.rs`, plus its `fuzz/Cargo.toml` `[[bin]]` entry, `fuzz/README.md` row and `fuzz/seeds/wikidata/`
- `crates/sources-plugins/src/wikidata.rs`, `crates/sources-plugins/fixtures/wikidata/**`, `crates/sources-plugins/tests/wikidata.rs`
- `docs/decisions/2026-11-12-ENR-14.md`

**Shared, and allowed small:**
- the `mod` lines and plugin registration in `sources-feeds/src/lib.rs` and `sources-plugins/src/lib.rs`;
- `docs/metrics.md` rows for any new metric;
- **ENR-14-harvest-count, from ENR-10:** the intake harvest count in `crates/pipeline/src/lib.rs` still counts an identification as harvested. Fix that counter, which is a metric only. Open PR #753 (UP-0) also edits `pipeline/src/lib.rs`, so keep your edit to that one counter and bring `main` in if they conflict.

**Don't edit** `crates/sources/src/identify.rs` or `crates/pipeline/src/identify.rs`: open PR #760 (ENR-12) changes both. If the harness needs a change for Wikidata, message the runner **before** writing it.

## Requirements
1. **ToS first (owner §2.28).** Check Wikidata's terms and its API and query-service usage policy:
   - rate limits;
   - a descriptive User-Agent;
   - no bulk scraping through the API;
   - licensing (CC0 data).

   Record what you checked, with dates, in the decisions file before any code that calls the service. Follow the strictest policy that applies. The User-Agent must not carry anything personal: no user name, email or host.
2. **Off by default, External class.** Nothing sends until the user enables the plugin. Use ENR-10's eligibility as is:
   - never a `flags::PRIVACY`, held or Adult-off item;
   - only what Q5 allows to be sent.

   **Key every outward gate on `flags::PRIVACY`, not only `PRIVATE_SOURCE`** (the runner's rule from UP-0).
3. **The parser is bounded and strict.**
   - Cap the bytes read per answer, and give the caps named constants.
   - Cap the counts: entities, labels and aliases per entity, and claims read.
   - Reject anything outside the shape you expect; don't guess.
   - No panics, unwraps, or truncating `as` casts on service data.
   - Parse ids strictly (`Q` followed by digits, with no overflow), and cross-ids (IMDb `tt…`, TMDB and TVDB numbers) to the known id kinds only.
   - Text goes through the same judge as every claim. Store nothing the judge holds.
4. **Queries are fixed.** Build the request from a fixed template plus validated ids or terms, never by concatenating user text into SPARQL. If you use the query service at all, the query text is a constant and the parameters are escaped or validated, and the decisions file says why it's safe. Prefer the entity and search APIs if they cover the need.
5. **Network.** Every request goes through the existing guarded client and SRC-5's egress and `source_state`, exactly as the other plugins do: timeouts, no redirects to other hosts, and the per-host policy from ENR-10.
6. **Tests and fuzzing.**
   - Recorded fixtures only, with no live calls in CI.
   - Cover: a match; no match; several candidates; the caps; garbage, oversize and truncated answers; a timeout; HTTP 429 back-off through ENR-10's limiter; a held text not stored; a private item never sent; the plugin off by default.
   - A fuzz target for the parser.
   - No fixed sleeps: paused tokio time, or condition waits with deadlines of at least 10 s. Never enable tokio's `test-util` on the `lodestar` package.
7. **The pattern note.** The decisions file ends with a short "How to add the next plugin" section: the files to copy, the caps to set, and the tests to have. It's for ENR-tmdb and the others.

## Not in scope
- UI.
- Indexing identification (ENR-11).
- Any other plugin.
- Changing ENR-10's harness without asking.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-sources-feeds -p lodestar-sources-plugins -p lodestar-pipeline`, and the workspace if disk allows;
- the fuzz target builds (`cargo check` in `fuzz/`);
- `/security-review` before the PR.

The PR description names the shared edits and the ToS findings. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review with security items follows.
