# UN-0: the usenet network key in the store, always private_source, never verified (Opus, core: store, /security-review)

**Why:** the first slice of v0.7's usenet tier. UN-1 through UN-4 build on it. Spec: `docs/plan/2026-11-05-v07-m7.md` §5 "UN-0", plan Q13 and Q16; `docs/architecture.md` §18.6 and §18.7. It may land before any cut, inactive: nothing writes usenet items until UN-1's plugin config is on. It runs in one of the runner's two extra sessions under owner §2.60.

## Build
1. **`Network::Usenet = 4`** in `crates/store/src/unverified.rs`. Today `unverified::Network` has only `Bt`.
   - **The key is SHA-256 of (indexer id, Newznab GUID): 32 B, length-prefixed or otherwise unambiguous,** so that ("ab","c") and ("a","bc") can't collide. Write the encoding in the decisions file.
   - V14's `unverified_items` CHECKs already allow networks 0–15 and keys up to 64 B, so **there's no migration** (Q16). If you find a CHECK that refuses it, stop and tell the runner; don't write a migration.
2. **The invariants, enforced in the store API, not by callers:**
   - a usenet item is **always `private_source`** (account sources, §18.7), whatever the caller passes;
   - it's **never promoted to verified**: any promote or verify path refuses `Network::Usenet` with a typed error;
   - it carries no info hash and never reaches DHT-facing code;
   - **purge per tier** (§18.6) covers usenet items.
3. **The cap per Q13:** 1M usenet items, oldest dropped first, the way sources-plan Q11 caps other unverified items. Make it a constant the runner can change, bounded in one transaction step per batch.
4. **Indexing:** usenet items are searchable only after SRC-3b, which is out of scope. Make sure the network decodes so `compile.rs`' `provenance` `network:usenet` term can be produced later. Don't touch `crates/search`.
5. **Tests** (`crates/store/tests/unverified_usenet.rs` plus unit tests):
   - round-trip a usenet item;
   - the key's determinism and its collision-freedom across the boundary;
   - private_source forced on;
   - promote refused;
   - purge per tier;
   - the cap evicting oldest first;
   - decoding unknown network values still errors as before;
   - an upgraded V14 database accepts the rows.
   No fixed sleeps.
6. **`/security-review`** on the diff before the PR. It's untrusted input from indexers through UN-1.
7. **Decisions file:** `docs/decisions/2026-11-12-UN-0.md`.

## Owns
- `crates/store/src/unverified.rs`
- `crates/store/tests/unverified_usenet.rs`
- its decisions file

**A shared one-line edit allowed:** a `pub use` in `crates/store/src/lib.rs` if needed. Ask the runner before anything else, and never edit `writer.rs`, `schema.rs` or the migrations.

## Checks
- `cargo fmt --check`
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
- `cargo test -p lodestar-store` (the workspace if disk allows; CI runs it)

Branch `slice/UN-0`; PR title "UN-0: usenet network key, always private_source, never verified". The plan's estimate is ~450 lines. Follow the steward skill, including a fresh self-review before the PR. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready <pr> <sha>" when CI is green. A fresh Opus reviewer with security items reviews it before merge.
