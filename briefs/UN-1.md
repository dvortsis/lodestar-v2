# UN-1: Prowlarr Newznab results as usenet items; the NZB link never stored

**Runner brief, 2026-11-12 (UTC).** Opus, core work (it parses untrusted input and handles credentials), `/security-review`.
- **Branch:** `slice/UN-1` from fresh `origin/main`, at or after 38827fd5.
- **PR title:** `UN-1: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Goal
When the user enables usenet results in the Prowlarr source, Lodestar stores Prowlarr's Newznab results as **usenet items** (UN-0's `unverified_items` rows), so UN-3 can later send them to SABnzbd or NZBGet. UN-2's adapters are in review (#757). Nothing in the UI or API shows these items yet: SRC-3b indexes them, and UN-3 and UN-4 expose them. Off by default.

## Read first
- `docs/plan/2026-11-05-v07-m7.md`: the UN-1 row, the usenet paragraph and Q13 (the cap).
- `docs/decisions/2026-11-12-UN-0.md`: how a usenet item is stored, its key, `network`, the 1M cap and evicting.
- `docs/decisions/2026-11-06-ENR-9.md` and `-ENR-9b.md`: the v3 claims. Follow ENR-9's mapping rule: no description or publisher beside a details section, and a Newznab poster can be an email address, so never store it as a name.
- `docs/decisions/2026-11-07-runner-followups.md`: the ingest judged-bytes budget, which applies.
- The UN-2 brief (runner-state `briefs/UN-2.md`) and `docs/decisions/2026-11-12-UN-2.md` once #757 merges: **send bytes, never a credentialed URL.**
- `crates/sources-plugins/src/prowlarr.rs` and `arr.rs` as they are today; `docs/architecture.md` §18.6 and §18.7; CLAUDE.md; `runner/standards.md`.

## Owns
- `crates/sources-plugins/src/prowlarr.rs`, `crates/sources-plugins/src/arr.rs`
- `crates/sources-plugins/tests/prowlarr_usenet.rs` (new)
- `crates/sources-plugins/fixtures/prowlarr/usenet/**` (new)
- `docs/decisions/2026-11-12-UN-1.md`

**Shared, and allowed small:**
- `crates/pipeline/src/unverified.rs` `judge_item`, about line 1308, for the re-judge fix in requirement 4 only. Keep it to the key-length rule plus its test. Tell the runner if it grows past about 40 lines.
- `docs/metrics.md` rows for new metrics.

Ask the runner **before** touching anything else, especially `crates/store/**`.

## Requirements
1. **The NZB link is never stored (the slice's title rule).**
   - Prowlarr's NZB download links carry the indexer's API key, and sometimes Prowlarr's own.
   - Store no URL that can fetch the NZB, and no part of one: no query strings, no keys, no `apikey` values, nothing in claims or logs. Not even redacted.
   - Store what UN-3 needs to ask Prowlarr again at send time (the indexer id, the guid, the title), and say exactly what in the decisions file.
   - `Debug` and `Display` of every type carrying a result never shows a link.
   - Add a test asserting that no stored byte and no log line contains the fixture's link or key.
2. **The mapping.**
   - Map Newznab fields per ENR-9's rule: title; size; category; posted date; group or indexer as provenance.
   - **Never the poster as a person name.** Drop it, or keep it only as judged text with no name role, and say which in the decisions file.
   - Every text is judged before storage. A held verdict stores nothing.
   - Key every outward gate on `flags::PRIVACY`, not only `PRIVATE_SOURCE` (the runner's rule from UP-0). A private indexer's items keep `private_source`.
3. **Bounds.**
   - Cap answer bytes and item counts per poll, with named constants.
   - Parse strictly: no panics, unwraps or truncating casts; sizes and dates are checked for overflow.
   - The ingest judged-bytes budget applies.
4. **The re-judge must not drop usenet items (UN-0's heads-up).** `judge_item` in `pipeline/src/unverified.rs` (about line 1322) removes keys that aren't 20 bytes when it re-judges. Usenet keys aren't 20 bytes. Judge usenet items by their claims, and never drop one for its key length. Add a test that re-judges a stored usenet item and keeps it.
5. **The cap's cost (UN-0's heads-up).** At 1M items, the cap's count-and-evict costs about 160 ms per adding batch inside the writer transaction.
   - Measure it with your poll sizes before you enable writes in the default config, and report the numbers.
   - If it's material, propose the fix (a `(network, id)` index in SRC-3b's migration, or a kept count) to the runner. **Don't change the store.**
6. **Known gaps to name, not fix:**
   - There's no store-level hold for usenet keys: a held item comes back on the next poll and is judged (and held) again. Name it beside follow-up 651.
   - From rv-748: undecodable usenet claims read `claims_private=false`, which is moot while the API writes flags 3.

   Put both in the decisions file.
7. **Tests** (`tests/prowlarr_usenet.rs` plus unit tests):
   - Recorded fixtures only.
   - Cover:
     - results stored as usenet items;
     - the off-by-default config stores nothing;
     - a private indexer keeps `private_source`;
     - the poster rule;
     - a held title stores nothing;
     - the caps, and oversize or garbage answers;
     - re-judging keeps a usenet item (requirement 4);
     - the link and key never stored or logged (requirement 1);
     - torrent results from the same source unchanged.
   - No fixed sleeps; condition waits have deadlines of at least 10 s. Never enable tokio's `test-util` on the `lodestar` package.

## Not in scope
- Sending to SABnzbd or NZBGet (UN-3).
- UI and API (UN-3, UN-4).
- Indexing (SRC-3b).
- The store schema.
- UN-2's adapters.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-sources-plugins -p lodestar-pipeline`, and the workspace if disk allows;
- `/security-review` before the PR.

The PR description names the shared edits, the never-stored rule and the cap measurement. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review with security items follows.
