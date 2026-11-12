# Brief: M8-NET-E2E (Opus, core), runner 2026-11-12

**Slice:** "In-process DHT network: ~50 nodes and fake peers, harvest -> fetch -> store -> index -> search within 5 s" (`runner/slices.yaml`; spec `docs/architecture.md` §17, the "In-process DHT test network" bullet; plan `docs/plan/2026-11-06-m8-hardening.md` §2 row "In-process DHT network").
**Branch:** `slice/M8-NET-E2E`. **PR title:** `M8-NET-E2E: <title>`. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
**Owns:** `crates/crawler-sim/tests/e2e_search.rs`, `crates/crawler-sim/tests/e2e/**`, `crates/crawler-sim/Cargo.toml`, `Cargo.lock`, and `docs/decisions/2026-11-12-M8-NET-E2E.md`. Nothing else; if production code needs a seam, stop and tell the runner first.

## What exists
- `crates/crawler-sim/tests/sim/` drives the real DHT actor and fetch pool against a scripted network (goldens). It covers DHT and fetch, not store, index or `/search`.
- Pipeline and store tests (`crates/pipeline/tests/pipeline.rs`, `stages.rs`) and search tests show how the real store, pipeline stages and the live indexer are assembled. Reuse their construction, don't copy production logic.

## What to build
1. **One test binary**, `tests/e2e_search.rs` (helpers under `tests/e2e/`), that spawns about 50 DHT nodes plus fake peers serving known info dicts (BEP 9/10 metadata), on loopback or the simulator's network, whichever the existing seams support for real store and index downstream. State the choice and why in the decisions file.
2. **The path under test is the production one:** harvest (the crawler sees the hashes announced/queried), fetch (metadata via the real peer-wire code), store (the real `lodestar-store` writer, a temp data dir), index (the real live indexer and `SearchIndex`), then a real search query (the search crate's query API or the `/search` handler, prefer the latter if it composes without a server). Assert every known torrent is returned by name, with correct size/file count, **within 5 s** of the fake peers starting serving.
3. **Timing:** a condition wait with a deadline (CLAUDE.md, no fixed sleeps). The 5 s figure is the spec's pass bar; the wait's own deadline may be longer (>= 10 s) so a miss reports how long it took, and the test then fails if that exceeded 5 s. Measure on the slowest CI target (Windows): if 5 s isn't met there, report numbers to the runner before changing anything; never loosen the bar yourself.
4. **Negative checks:** a hash with no peer never becomes searchable; a peer serving an info dict whose SHA-1 doesn't match is rejected (nothing stored).
5. **Deterministic and quiet:** seeded; no outbound network; bounded runtime (aim < 30 s per run); runs in plain `cargo test --workspace`. Run it 10 times locally and report flake count.
6. **Dev-dependencies only** in `crates/crawler-sim/Cargo.toml` (search, pipeline, api, as needed). Keep tokio's existing features; don't add tokio `test-util`/`testing` to any other package. Approved: adding workspace crates as dev-deps of crawler-sim (standards §2). No new third-party crates without asking.
7. **Decisions file:** network choice, which production components are real vs faked and why, the measured time on each OS from CI, and that M8-CHAOS-1 builds on this harness (Cargo.lock chain: NET-E2E -> CHAOS-1 -> PERF-2).

## Checks
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Reply to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready", the PR number and the full 40-char head SHA once CI is green, plus the measured times (Linux, macOS, Windows) from CI logs.
