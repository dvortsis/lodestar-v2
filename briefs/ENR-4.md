# ENR-4: the ThePornDB plugin (Account class, off by default, not registered)
**Model:** Sonnet (glue on top of Opus-built parts). `/security-review`, then a fresh Opus reviewer. Runner brief, 2026-11-05T15:55Z.

## Why
ENR-1 (#480) and ENR-1b (#497) built the adult-enrichment runtime and its gate. ENR-2b (#440) built the bounded ThePornDB response parser. ENR-4 is the plugin between them. It's on the ENR-G path (ENR-4 and ENR-5, then ENR-G).

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md` §4.3, "ENR-4 / ENR-5", plus Q2, Q4, Q5 and Q15.
- `docs/decisions/2026-11-05-ENR-1.md`, the plugin contract: `AdultEnricher`, `Query` and `QueryForm`, `lookup(kit, query)`.
- `docs/decisions/2026-11-05-ENR-1b.md` (merged), and, if it has merged by then, ENR-1c's.
- `docs/decisions/2026-11-04-ENR-2b.md` (the parser: what it keeps and what it never returns).
- `crates/sources-plugins/src/prowlarr.rs`: SRC-7's pattern for Account-class manifests, sealed credentials, fixtures and contract tests.
- The HOLD pattern in `docs/decisions/2026-11-05-SRC-10e.md`.

## Do (branch `slice/ENR-4` from `origin/main`)
1. **The plugin** goes in `crates/sources-plugins/src/tpdb.rs`. It implements `AdultEnricher`.
   - **Manifest:** `Kind::Enrichment`; the **Account** consent class; endpoints on ThePornDB's API host only; `distribution: coordinator`. Use the caps from Q15 and ENR-1. The runtime already paces lookups, so don't add a second rate limiter; state the per-host limits in the manifest the way ENR-1 expects.
   - **Config schema:** the API key, through SRC-4's sealed per-source credentials. It is never returned, logged, put in `Debug` or exported, and it's dropped on purge.
   - **Requests:** built only from a `Query`, the display name or the parsed site, date and title. Never an info hash, file name, tracker or anything about the install. They never request performer ids or personal fields (ENR-2b, Q4, Q5). Prefer the query endpoints; never use `/scenes/hash/{hash}`.
   - **Mapping:** `sources_feeds::tpdb::parse` output to ENR-0's section fields. Take `found.without_performers()` or keep performers exactly as Q4 and ENR-0 allow.
   - **Errors:** read the HTTP status. A 401 or 403 means a bad key: say so without echoing the key. A 429 backs off. The service's message text is never kept.
2. **Not registered.** Don't add it to `crates/lodestar/src/services.rs` or to any registry the host builds, and don't make it enable-able from Settings. Put a **HOLD** at the top of your decisions file, as SRC-10e did:
   - the Q2 terms and robots check for ThePornDB's API is owed by the owner;
   - host wiring, which builds `AdultEnrichSource` with its Switches and StoreState and hands `Interest` to ENR-7's API, is a later slice.
   - One `mod tpdb;` line in `crates/sources-plugins/src/lib.rs` is the only shared edit.
3. **Fixtures and tests** (`crates/sources-plugins/fixtures/tpdb/**`, `crates/sources-plugins/tests/tpdb.rs`). Fixtures are recorded-style JSON with no real key; you may write them by hand from ENR-2b's fixtures. Follow SRC-7's contract tests:
   - fixture replay;
   - cancellation;
   - caps;
   - the blocklist applies;
   - no request carries an info hash, a file name or the key in the URL (the key goes in a header);
   - enabling without a key is refused (Account class);
   - the key never appears in `Debug`, errors or logs;
   - a 401, a 429 and a bad body each end in the right outcome.

   No live network, and no fixed sleeps.

## Own
`crates/sources-plugins/src/tpdb.rs`, `crates/sources-plugins/fixtures/tpdb/**`, `crates/sources-plugins/tests/tpdb.rs`, and `docs/decisions/2026-11-05-ENR-4.md`. The `mod` line in `lib.rs` is shared.

**Don't touch:**
- `crates/sources-feeds/**`: the parser is done. If it lacks something, tell the runner.
- `crates/pipeline/**`: ENR-1c is running there.
- `crates/api/**`: ENR-7.
- `crates/lodestar/**`: host wiring.
- `crates/sources-plugins/src/stashdb.rs`: ENR-5.

## Done when
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- `/security-review` on the diff.

Then:
- Open the PR "ENR-4: ThePornDB plugin (Account class, off by default)" with the label `full-ci`.
- Follow `.claude/skills/steward/SKILL.md`.
- Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool.

Never merge. Push once per round. No fixed sleeps.
