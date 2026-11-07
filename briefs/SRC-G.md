# SRC-G: the v0.6 sources gate
**Model:** Opus, with `/security-review`. A fresh Opus reviewer follows.

Runner brief, 2026-11-07T15:13Z. Slot 2. Track S (ladder rung 3): the gate for E-v06-sources.

## Read first
- `docs/plan/2026-11-01-v06-sources.md` §4.2 (the SRC-G line and every slice it depends on), §5 (answered questions) and the verification summary near the end.
- `docs/architecture.md` §18.7 (consent classes, `private_source`), §18.9 (contract tests, the gate), §3 (overhead budgets).
- `runner/slices.yaml`, SRC-G's notes. The runner's security pre-audit (2026-11-05) requires a **store-backed, fail-closed `PrivateTrackers`** before `full_scrape` is wired; only the test-only `StaticPrivateTrackers` exists. Also owner §2.37: the full-scrape settings text and user guide tell the user not to add a public tracker whose published rules forbid scraping.
- `docs/plan/2026-11-06-v052-feature-manifest.md` §3: which plugins are registered in the binary today (DHT, torrent cache, Sonarr/Radarr webhooks) and which are compiled in but excluded.

## What the gate is
A test suite plus a verdict. It proves the v0.6 sources safety properties across **every plugin kind** in `crates/sources-plugins` (academic, archive_org, arr, clients, full_scrape, nyaa/sukebei, prowlarr, stashdb, torrent_cache, tpdb, and the DHT), through the real pipeline and store where the property lives there, not only per plugin.

## Do
1. **Blocklist through every plugin kind:** a blocklisted fixture hash and a blocklisted name, injected through each plugin's recorded fixture, are dropped before storage and counted, never stored, never shown, never promoted. Covers claims and unverified items, and the after-storage re-judge.
2. **`private_source` isolation:** a key found only by a private source never appears in any output that exists today: feeds/RSS, integrations hand-off and webhooks out, shareable exports, the search API for a non-admin view if one exists, logs at info, and **no public lookup** (DHT get_peers/announce, PEX, the torrent cache's lookups, trackers other than its own). Outputs that don't exist yet (Torznab, Stremio): say so in the verdict and leave a placeholder test that fails closed when the module appears, if that can be done cheaply; otherwise list it for their slices.
3. **Unverified hidden by default:** `verified:no` items stay out of default search, counts, Insights and feeds unless asked for.
4. **Consent can't be bypassed:** for each consent class, every enable path (API `PATCH`, settings import, config file, startup restore of a saved state, a manifest version change) refuses until its condition holds; a raised `consent::text_version` re-asks. Include X-consent-gate-052's v0.5.2 rule (classes not yet shown by the dialog stay refused).
5. **No-profiles:** the schema and a log scan show no per-user or per-peer profile is kept (the §18 rule): no table or column joins a peer address to torrents over time, and a log scan over a fixture run finds no peer-address-to-infohash lines.
6. **`PrivateTrackers`:** if `full_scrape` is wired anywhere, it must be wired with a store-backed, fail-closed `PrivateTrackers`. Today it should be unwired; assert that (a test that fails if `full_scrape` is registered without it). Check the §2.37 text exists in the settings help and the user guide, or list it as a blocker.
7. **Overhead bench** (`bench/tests/sources_overhead.rs`): every v0.6 source on against fixtures, measured against the §3 budgets. Report numbers; a budget miss goes to the runner before anything else.
8. **e2e** (`web/e2e/sources.spec.ts`): the Sources screen shows consent state and refuses to enable without it, and unverified badges show, as far as the built UI supports.
9. **Verdict** in `docs/decisions/2026-11-07-SRC-G.md`: each property PASS / FAIL / NOT-YET-BUILT with the test that proves it. A FAIL is a defect: fix it in this PR only if it is small and inside a file you own or the runner approves; otherwise stop, report it to the runner with file:line and a failing test, and keep the test (marked to run, not ignored; the PR then waits for the fix slice).

## Owns
- `crates/sources-plugins/tests/safety.rs` (new), `bench/tests/sources_overhead.rs` (new), `web/e2e/sources.spec.ts` (new), `docs/decisions/2026-11-07-SRC-G.md`.
- New test-support code under `crates/sources-plugins/tests/` or `crates/pipeline/tests/` if needed.
- **Ask the runner first** before editing any non-test file, naming the function. **Don't touch:** `crates/sources/src/pool.rs` and `crates/peer-wire/**` (#579), `crates/cluster/src/leases.rs`, `crates/pipeline/src/{coordinator,rollups,remote}.rs` and `crates/api/src/insights*` (WK-15), `crates/api/src/legal/**`, `docs/legal/**`, `crates/lodestar/tests/{legal_facts,webhooks_e2e}.rs` and `packaging/**/smoke.*` (P-v052-cut).

## Done when
- Every property above has a test or a NOT-YET-BUILT entry with reason; the verdict file is complete.
- No live network in tests (fixtures only). No fixed sleeps; waits on conditions with deadlines ≥ 10 s. No tokio `test-util` or `testing` features on the `lodestar` package.
- Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per-package runs fine on limited disk; say so), and in `web/`: `npm run check`, `npx playwright test e2e/sources.spec.ts` with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (never `playwright install`).
- Size: aim ≲ 1,200 lines; split by property group if larger.
- Process: follow `.claude/skills/steward/SKILL.md`, push once per round, never merge. PR title `SRC-G: v0.6 sources safety gate`, branch `slice/SRC-G`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green, and at once for any FAIL.
