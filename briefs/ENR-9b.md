# ENR-9b: Store v3 claim details through store_claims, judged before storage
**Model:** Opus, with `/security-review`. This is content-safety code: claim text goes into the store. A fresh Opus reviewer follows. Runner brief, 2026-11-06T21:35Z, launched into the slot WK-1 freed (#544 merged), within owner §2.42's four slots.

## Spec
- `docs/decisions/2026-11-06-ENR-9.md` in full, above all "No personal fields", "Clamping" and "Outside the brief's owns" (the runner names this slice to turn the mapping on). The plan is `docs/plan/2026-11-05-v07-m7.md` (the ENR-9/ENR-10 rows and the "may land before the cut, inactive" rule). Owner §2.37 and §2.46/§2.47 (v0.7 Q5).
- The `runner/slices.yaml` entry `ENR-9b` and its notes. ENR-10, UN-1 and IRC-1 depend on this slice.
- `crates/pipeline/src/unverified.rs`: `store_claims` (today `details: None`), `judge_in`/`judge`, the refile path, and the #383 rule that **every claim text that is stored passes the filter**.

## Do
1. **`store_claims` maps the clamped details section** (`lodestar_sources::Claims::clamp` already applies the rules) into the store's v3 `details`, under the rules below.
   - **Never when the claims carry an adult section** (clamp drops it there; assert that, don't rely on it).
   - **Never beside a description or publisher**, so the store's `details claims` refusal can't fire on a valid input.
   - Think about `private_source`: if a details field could leak private-tracker data to public surfaces, it follows the same rule as the other fields there. Record your reading.
2. **The section is judged before storage, like the adult section.** Every free-text field is judged as a name through the same full filter pipeline (locked floor, core, the user's rules) that judges the claimed name and paths: the canonical title, each alternative title, the source category, genres and newsgroups. Use the same verdict combination (the stricter verdict stands).
   - **A block** on any of them drops or holds the key exactly as a blocked name does today.
   - Ids, years and confidence aren't text. Say so in a doc comment.
3. **Refile:** claims re-judged at start and when the filter engine's version changes also re-judge the stored details text. Show it with a test: a rule added after storage blocks a key through an alternative title alone.
4. **Inactive no more:** `crates/pipeline/tests/claims_v3_inactive.rs` asserted that v3 is never written. Rewrite or replace it with `crates/pipeline/tests/unverified_details.rs`:
   - v1 and v2 inputs still store byte-identical blobs;
   - a details section stores as v3 and round-trips;
   - the adult-section and personal-field cases;
   - a blocked alternative title or category blocks the key;
   - a held one holds it;
   - the bounded sizes (`claims_kib` counts the section).
5. **Property test:** whatever a source sends, `store_claims` either stores claims the store accepts or drops the section. It never yields an error or a panic.
6. **The decisions file** `docs/decisions/2026-11-06-ENR-9b.md`, including notes for ENR-10, UN-1 and IRC-1.

## Own
- `crates/pipeline/src/unverified.rs`
- `crates/pipeline/tests/unverified_details.rs`
- `crates/pipeline/tests/claims_v3_inactive.rs`, to rewrite or remove
- the decisions file above
- this slice's own `runner/slices.yaml` entry (`owns` only)

Anything else (`crates/filter/**`, `crates/store/**`, `crates/sources/**`): message the runner first. **Running beside it:** WK-7 (#549, `crates/pipeline/src/{stages,worker}.rs`), CMP-0 (#548, `crates/companions`) and SRC-15b (`web/`). None overlaps.

## Done when
- **These pass:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-pipeline`, plus dependants if you touch a public API;
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- **No fixed sleeps.**
- **Branch and PR:** branch `slice/ENR-9b`, PR "ENR-9b: <short imperative title>".
- **Steward rules:** follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA via the claude-code-remote `send_message` tool.
