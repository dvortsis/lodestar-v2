# F-suggest-adult-field: `/suggest` must not offer `adult:` while Adult is hidden
**Model:** Opus (content-safety tightening in the search crate). Run `/security-review`. The runner reviews. Runner brief, 2026-11-01. Found in the #335 review (B-search-meta).

## Why
§20.9: nothing Adult is shown unless "Show adult content" is on. `GET /search/fields` (#335) already leaves out the `adult:` field and `type:adult` while Adult is hidden. But `/suggest`'s `field_names` (`crates/search/src/suggest/mod.rs`, ~l.336) lists every field in `query::FIELDS`, `adult` included, so typing `ad` offers `adult:` with Adult hidden.

## Read first
- `docs/architecture.md` §20.3, §20.9; `docs/decisions/2026-11-01-B-search-meta.md`.
- `crates/search/src/suggest/mod.rs` (`field_names`, `values`, the `Shown` visibility the request carries), `crates/search/src/catalogue.rs` (how #335 hides Adult), `crates/api/tests/suggest.rs`.

## Do
1. With Adult hidden, `/suggest` never offers:
   - the `adult:` field name;
   - any `adult:` value (if a user types `adult:` by hand);
   - `type:adult`, or any chip or did-you-mean that rewrites to an Adult clause.

   Check each path in `suggest/` (field names, values, chips, did-you-mean, titles) and say in the decisions file which ones already held and which you changed.
2. With Adult shown, nothing changes.
3. Only tighten: no path that showed nothing Adult may start showing something.
4. **Tests:** a unit test per path you changed (each fails with its guard removed), and an API test in `crates/api/tests/suggest.rs` for `q=ad` with Adult hidden and shown.
5. **Decisions file:** `docs/decisions/<date>-F-suggest-adult-field.md` (short).

## Own
`crates/search/src/suggest/**`, `crates/api/tests/suggest.rs` (and its goldens, if any change), your decisions file. **Don't edit** `catalogue.rs` or the query parser.

## Size
≲ 200 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `search-bench --gate` (`/suggest` p95 unchanged within noise). Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
