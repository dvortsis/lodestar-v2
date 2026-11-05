# ENR-5: the StashDB plugin (Account class, off by default, not registered)
**Model:** Sonnet (glue over Opus-built parts). `/security-review`, then a fresh Opus reviewer. Runner brief, 2026-11-05T18:17Z.

## Why
This is the StashDB twin of ENR-4 (#503, ThePornDB). The runtime (ENR-1, ENR-1b, ENR-1c) and the bounded StashDB GraphQL parser (ENR-3, #438) are already built. ENR-5 is the plugin that sits between them, on the ENR-G path.

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md` §4.3 "ENR-4 / ENR-5", plus Q2, Q4, Q5 and Q15.
- `docs/decisions/2026-11-05-ENR-1.md`, `2026-11-05-ENR-1b.md`, and ENR-1c's file once #502 merges (one shared daily cap; adult enrichers are listed explicitly).
- `docs/decisions/2026-11-04-ENR-3.md` (the parser: fixed query documents, what it keeps and never returns).
- **ENR-4's PR #503 and its decisions file `docs/decisions/2026-11-05-ENR-4.md`.** Mirror its structure. Also take these lessons from its review:
  - **Use the service's top result or nothing:** set `max_items` to 1, so an unreadable top result is an error, never the second-best guess. The second-best guess could attach another scene's performers to the torrent (Q4).
  - **Never cache an error object as a miss:** only a real empty or null result is a miss. If ENR-3's parser can't report that it saw the result field, check it structurally (as ENR-4's `data_start` does), and put the proper parser flag in your HOLD as a precondition for registration.
  - **Keep the key out of every leak path:** it goes only in a header, and never in the URL, `Debug`, errors, logs or exports. A 401 or 403 must not echo it. Add a key-leak test that runs every outcome (401, 403, 429, 500, a bad body, HTML, a redirect) with a distinctive key.
  - **Refuse redirects:** add a 3xx fixture with an off-host `Location` and check it makes one request ending in FAILED.
  - **Assert the exact header list,** and assert that the query text doesn't appear in the request's `Debug`.
- `crates/sources-plugins/src/prowlarr.rs`: SRC-7's contract-test pattern.

## Do (branch `slice/ENR-5` from `origin/main`)
1. **The plugin** in `crates/sources-plugins/src/stashdb.rs` implements `AdultEnricher`.
   - Manifest: `Kind::Enrichment`, the Account class, `always_adult: false`, endpoints on StashDB's GraphQL host only, `distribution: coordinator`, and Q15's per-host pacing.
   - The API key goes through SRC-4 sealed credentials, in StashDB's `ApiKey` header.
   - Requests are built only from a `Query` (the display name, or the parsed site, date and title), using ENR-3's fixed query documents. They never contain an info hash, a file name, a tracker, performer ids or personal fields.
   - Map the results to ENR-0's section fields.
2. **Don't register it** anywhere: not in `crates/lodestar/**` and not in any registry the host builds. Put a **HOLD** at the top of the decisions file:
   - the owner still owes the Q2 terms and robots check for StashDB's API;
   - host wiring is a later slice;
   - any parser flag you need is a precondition for registration.
3. **Fixtures and tests** go in `crates/sources-plugins/fixtures/stashdb/**` and `crates/sources-plugins/tests/stashdb.rs`. Cover:
   - fixture replay, cancellation and caps;
   - no info hash, file name or key in the URL or body except the key header;
   - enabling without a key is refused;
   - the key-leak test, the redirect test and the top-result-only test above.

   No live network and no fixed sleeps. The existing test that no manifest is both Enrichment and always-adult must keep passing with yours included.

## Own
- `crates/sources-plugins/src/stashdb.rs`
- `crates/sources-plugins/fixtures/stashdb/**`
- `crates/sources-plugins/tests/stashdb.rs`
- `docs/decisions/2026-11-05-ENR-5.md`

**Shared:** one `mod` line in `lib.rs`. ENR-4 (#503, open) adds its own; whichever merges later brings `main` in.

**Don't touch:** `crates/sources-feeds/**` (tell the runner if the parser lacks something), `crates/pipeline/**` (ENR-1c), `crates/api/**` (ENR-7), `crates/lodestar/**` and `tpdb.rs`.

## Done when
`cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`, or at least the crate tests if disk runs short (say which); and **`/security-review` on the diff. It is required, and a self-review doesn't replace it.**

Open the PR "ENR-5: StashDB plugin (Account class, off by default)" with the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
