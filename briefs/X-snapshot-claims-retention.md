# X-snapshot-claims-retention: migration snapshots never keep identify claims past their source's retention

**Model:** Opus (store, on-disk data, terms compliance). **Branch:** `slice/X-snapshot-claims-retention`. **PR title:** `X-snapshot-claims-retention: <title>`.

## Why
`docs/decisions/2026-11-16-ENR-terms.md` (line 84, "Migration snapshots keep claims past their retention") records the gap. A migration snapshot (`lodestar.pre-v<N>.db`) is kept for 90 days (`RETAIN_SECS`, `crates/store/src/snapshots.rs`) and holds identify claims as they were. A claim near its source's retention limit therefore outlives that limit inside the snapshot, by up to 90 days. That would break TMDB's six-month term. The runner decided (2026-11-16) that this slice must merge **before ENR-tmdb (#948)**.

## What to build
1. **The snapshot purge pass drops expired identify claims** (`Store::purge_snapshots` and its plan in `snapshots.rs`). For every source that has a terms record in the snapshot (`enr10/terms` in `source_state`, read through the same `Terms::from_record` rules as `identify::sweep_recorded` in `crates/pipeline/src/identify.rs`), remove claims at or past the record's retention. An unreadable record falls back to `Retention::DEFAULT`. Use the snapshot's own high-water clock (`enr10/clock`) the way the live sweep does, so a clock that steps back can't keep a claim. The purge must be at least as strict as the live sweep, never looser. A snapshot whose claims can't be checked fails closed: those claims go.
2. **Restore and rollback stay correct.** Look at `purge_for_restore` and `crates/lodestar/src/cli/rollback.rs`. A restored snapshot must never bring back a claim that the live store's rules would already have dropped. Cover it with a test, or say why it can't happen.
3. **Bounded work**, in keeping with the existing purge: chunked deletes by key (the bundled SQLite has no `DELETE … LIMIT`), resumable, inside the pass's existing time and IO budget. State any new bound in the decisions file.
4. **The dependency direction stays as it is.** `lodestar-store` must not depend on `lodestar-pipeline`. If the terms-record parser is needed in the store, move the minimal pure parser to where both can use it (for example `crates/sources/src/terms.rs`, if the store already depends on that crate; otherwise the store gets a small, documented reader of the stored record format with a test that pins it to `Terms::record()`). Say which in the decisions file.

## Tests
- Claims of a source with a short retention are purged from a snapshot once past it. Claims within retention, other sources' claims, and non-identify data are untouched.
- A clock that steps back (the snapshot's high-water mark ahead of now) still drops claims.
- An unreadable or missing terms record: the default retention, or fail closed, as you decide. Test it and state it.
- Resumable: an interrupted pass completes on the next pass with the same result.
- A restore of a snapshot doesn't resurrect an expired claim.
- A property test over random retention, claim stamps and clock sequences, in the style of ENR-terms' identify property test.
- No fixed sleeps; condition waits need deadlines of at least 10 s.

## Rules
- **Owns:** `crates/store/src/snapshots.rs`, any new `crates/store/src/snapshots/**` module, store tests for it, `docs/decisions/<date>-X-snapshot-claims-retention.md`. You may also touch, as small shared edits: `crates/store/src/schema.rs` (the snapshot plan only, if needed), the terms-record reader as in item 4, and `crates/lodestar/src/cli/rollback.rs` if restore needs it. Ask the runner before anything else.
- **No schema migration** unless unavoidable. If one is needed, ask the runner for the number first.
- `unsafe_code` stays forbidden.
- **Running alongside you:** CR-LEDGER (crates/dht), X-crawler-sim-calibrate (crates/crawler-sim), GS-3 (search, api). The API lane's ENR-tmdb (#948) waits for you; don't edit its files.
- **Before the PR:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and tests for lodestar-store, lodestar-pipeline and lodestar.
- ≲ 800 changed lines.
- **Review:** a fresh Opus review plus /security-review (terms compliance and data retention).
- Use the steward skill. Report `ready <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.
