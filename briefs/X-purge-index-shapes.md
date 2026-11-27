# X-purge-index-shapes: A purge takes served identification data out of the index

**Runner brief, 2026-11-27T08:30Z.** Core, Opus (storage and the index's serving rule). Target < 800 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-purge-index-shapes` from fresh `origin/main` (with #1133 merged as 1b46c223).
- **PR title:** `X-purge-index-shapes: A purge takes served identification data out of the index`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- The `runner/slices.yaml` entry already exists (r18). Don't add another; edit nothing else in that file or `runner/dashboard.yaml`.

## Why
The runner's audit of #1117 (ENR-G) found, as an important latent and public issue, that purging a source can leave identification data that came from it in the search index: alternative titles and ID terms keep being served after the claim that supplied them is gone. Two predicates should agree and don't: the served identification rule (`Store::served_identifications`, `crates/store/src/identify_sweep.rs`) and the claims-shape rule (`Claims::shapes_the_record`, `crates/store/src/claims.rs`).

## Read first
`docs/decisions/2026-11-24-ENR-G.md`, `docs/decisions/2026-11-24-ENR-11.md`, the X-purge-rederive decisions file (#1120), architecture §20.5 and §20.9, `crates/store/src/identify_sweep.rs`, `crates/store/src/claims.rs`, `crates/store/src/unverified.rs`, how a purge reaches the store (search for the source purge path, including `crates/store/src/snapshots.rs` `purge_claims_only`), how `reindex_log` drives reindexing, and `runner/standards.md`.

## Do
1. Make the served identification predicate match the claims-shape predicate exactly: an answer is served only while a claim that shapes the record supplies it. One shared function or a property test that holds the two together over every claim shape; say which in the decisions file.
2. A purge that removes such a claim logs the torrent to `reindex_log` in the same transaction, so the index drops the data at its next pass. A crash between the purge and the reindex must still leave the torrent logged.
3. Tests: a torrent identified only by a purged source serves no identification terms after the reindex (`crates/search/tests/enrichment.rs`); one identified by two sources keeps the survivor's answer; the transaction property (purge and log together or neither); the predicate agreement test.
4. If the purge path lives outside your owns (for example `snapshots.rs`), message the runner with the exact lines before editing.

## Owns
`crates/store/src/identify_sweep.rs`, `crates/store/src/claims.rs`, `crates/store/src/unverified.rs`, `crates/store/tests/unverified.rs`, `crates/search/tests/enrichment.rs`, `docs/decisions/*-X-purge-index-shapes.md`. Not `crates/dht/**`, `crates/lodestar/**` (CR-0w), `crates/api/src/torznab*` (X-torznab-id-caps), `crates/sources/**`, `crates/integrations/**`, `crates/tracker-scrape/**` (X-sec-S8-25).

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (if the disk allowance can't hold a full workspace build, run the store and search crates and say so). No fixed sleeps. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
