# GS-0: the payload_sig producer and the `sig:` query field (Opus, core: search)

**Why:** the first producer of v0.7's grouping chain. GS-1 through GS-5 all depend on it, so it's the longest path in M7. Spec: `docs/plan/2026-11-05-v07-m7.md` §3 (the `payload_sig` rows) and §5 "GS-0"; `docs/architecture.md` §6 (grouping and dedup) and §8; build-plan Track M7. It runs in one of the runner's two extra sessions under owner §2.60.

## Build
1. **Move the hash to `classify`.** Add `crates/classify/src/payload.rs`: the `payload_sig` of the non-padding file sizes, using `fmix64` as `filter::hash` does, and **byte-for-byte the same value**. `classify` returns the `u64`. `filter::payload_sig` keeps its `PayloadSig { sig, specific }` shape and CS-8a's `specific` rule on top, and re-exports or calls the moved function. CS-8a's behaviour is unchanged, and a golden test proves it.
2. **Write it in `crates/search/src/doc.rs`** as the field defined at generation 0, with a u64 fast column and the term. Bump it to **generation 1** under SB-6's population contract (`population.rs`): the generation bump, the backfill walk, and "partial while backfilling".
3. **The `sig:` query field** (`query.rs`, `compile.rs`):
   - `sig:<16 hex>`;
   - `sig:` with a pasted info hash ("same payload as this torrent"), resolved through `SearchIndex::prefilter`;
   - reported partial while the walk runs. It never answers "no match" on an unpopulated document.
   - It replaces today's "arrives with grouping keys (M7)" refusal (`query.rs` ~457).
   - Errors name the cause without echoing user text beyond what's already allowed.
4. **Size:** run `size-report --torrents 50000` and put the per-torrent bytes in the PR. The plan estimates ~16–20 B. GS-gate's `size-report --gate` must stay green. If it fails the 512 B budget, stop and tell the runner (plan Q10). Don't raise any budget yourself.
5. **Tests:**
   - unit tests for the moved hash: equal to the old one for a corpus of file lists, padding ignored, order-independent if the old one is;
   - the SB-6 **population acceptance test**: SB-6, then import, then the producer, with a restart mid-walk;
   - `sig:` on populated and unpopulated documents;
   - the CS-8a golden unchanged;
   - a property test that `classify::payload_sig` equals the old `filter` computation on generated file lists.
   - Condition waits with deadlines of at least 10 s. No fixed sleeps, and no tokio `test-util`.
6. **Decisions file:** `docs/decisions/2026-11-12-GS-0.md`. Mark any refinement for the runner to flag (§2.19).

## Owns
- `crates/classify/src/payload.rs`, `crates/filter/src/payload.rs`
- `crates/search/src/doc.rs`, `crates/search/src/population.rs`, `crates/search/src/query.rs`, `crates/search/src/compile.rs`
- `crates/search/tests/payload_sig.rs`
- its decisions file, plus its own `runner/slices.yaml` gates entry if one names GS-0

**Shared one-line edits allowed without asking:** the `mod` line in `crates/classify/src/lib.rs`, a `classify` dependency line in `crates/search/Cargo.toml` or `crates/filter/Cargo.toml` and its `Cargo.lock` line, and `docs/metrics.md` or `docs/ui-glossary.md` rows if you add a field name users see. Ask the runner before editing anything else.

## Not owned
- **Open #717 (RX-0)** touches `crates/search/src/lib.rs` (the `regex` mod line), `regex.rs` and `Cargo.toml`. If you also need `lib.rs`, it's a trivial conflict, and whichever lands later brings main in.
- **Never edit** `writer.rs`, `extract.rs` or any store migration.

## Checks
- `cargo fmt --check`
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
- `cargo test -p lodestar-classify -p lodestar-filter -p lodestar-search` (the workspace if disk allows; CI runs it)
- GS-gate's size gate.

Branch `slice/GS-0`; PR title "GS-0: payload_sig producer and sig: query field". Keep it ≲ 1,000 changed lines; the plan's estimate is ~650. Follow the steward skill, including a fresh self-review before the PR. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready <pr> <sha>" when ci-ok, web-ok and e2e-ok are green. A fresh Opus reviewer reviews it before merge.
