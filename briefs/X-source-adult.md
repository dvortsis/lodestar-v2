# X-source-adult: a source-level always-adult signal, applied as an Is-adult classification
**Model:** Opus (content safety; pipeline classification). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Why
Owner Q7 (Sukebei): "every item forced into the Adult category, hidden unless Show adult content is on." SRC-10e (#461) maps every Sukebei find with an `adult` content-type claim, but **nothing in the pipeline reads a `content-type` claim**, so a clean-named item from that source would be stored as ordinary content and shown with Show adult content off. The plugin stays unregistered until this lands.

## Do
1. **The signal:** a manifest-level flag in `crates/sources` (e.g. `always_adult: bool` on the plugin manifest, default false; set true for SRC-10e's `SUKEBEI_MANIFEST`). It's a property of the source, not of an item's claims, so a misbehaving feed can't turn it off. Prefer it over trusting the `content-type` claim; if you also honour that claim, it may only ever make an item adult, never non-adult.
2. **Apply it as an Is-adult classification** (the same effect as an Is-adult correction: `ContentType::Adult` with the nearest adult kind, `crates/classify` `lib.rs`/`adult.rs`), stricter only, at every point an item from such a source is classified or re-classified:
   - the unverified item and its claims when stored (`crates/pipeline/src/unverified.rs`);
   - promotion when real metadata arrives later (BEP 9, the worker's classify path in `worker.rs`), so a later clean name can't un-adult it;
   - re-judging sweeps (writer refile) keep it adult.
   An item seen from both an always-adult source and the DHT stays adult (stricter wins). A user's Not-adult correction on such an item: decide and record (recommend: allowed, as for any item, since it's the user's own explicit act; flag it under §2.19).
3. **Tests:** a clean-named item from an always-adult source is stored Adult and is hidden from search, feeds and exports with Show adult content off and shown with it on; promotion keeps it adult; a non-adult source is unaffected; the floor and child-safety core still apply first; the adult-only core rules (CS-5a etc.) apply to it. No fixed sleeps.
4. Fold in from the #461 review: give Sukebei its own category claim scheme (`sukebei-category`, not `nyaa-category`), fix SRC-10e's decisions-file wording ("the scrub grep finds nothing"), and rename the `sukebei_blocked` fixture to what it tests.

## Own
`crates/sources/src` (the manifest flag and its docs), `crates/pipeline/src/unverified.rs`, `crates/pipeline/src/worker.rs` (the classify path only), `crates/sources-plugins/src/nyaa.rs` (set the flag on SUKEBEI_MANIFEST; the category scheme), `crates/sources-plugins/tests/sukebei.rs`, the fixture rename, a new `crates/pipeline/tests/source_adult.rs`, and `docs/decisions/2026-11-05-X-source-adult.md`. Don't touch `crates/filter/**` (RP-2 is running) or `crates/pipeline/src/torrent_cache.rs` (SRC-13).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `/security-review`. PR "X-source-adult: …" against `main` from `slice/X-source-adult`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round.
