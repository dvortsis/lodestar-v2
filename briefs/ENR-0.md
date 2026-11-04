# ENR-0: Claim encoding v2 with an optional adult-enrichment section
**Model:** Opus, with `/security-review`. This is an on-disk format that decodes untrusted, stored bytes. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33.

## Spec
- **Plan:** `docs/plan/2026-11-04-v06-rulepacks.md` §4.3, the ENR-0 bullet (around line 147), and line 49 (where enrichment results live).
- **The owner's answers:** `docs/owner-decisions.md` §2.37. The decision is the plan's §5 table, Q2-Q5:
  - performer names are shown on the item only;
  - no per-performer pages or cross-item lists in v0.6;
  - no other personal fields;
  - no birth dates;
  - descriptions are neither fetched nor stored.
- **Architecture:** §18.1, §18.5 (claims), §20.9.
- **The current code:** `crates/store/src/claims.rs` (encoding v1, from SRC-1) and `crates/sources/src/claims.rs` (the claim type).

## Runner calls (2026-11-04, flagged to the owner under §2.19)
- **No performer ids**, names only. Lodestar asks for less, under Q4: an id is the key that would link a performer across items. The StashDB and ThePornDB parsers (ENR-3 #438 and ENR-2b #440) already keep names only.
- **Store the studio name only.** ENR-2b doesn't read a studio id, so there's nothing to keep.
- **Parsers degrade rather than spoil.** A performer list can be cut short, so v2 carries a `performers_truncated` flag.

## Do
1. **Encoding version 2** = all of version 1's fields, plus an optional adult-enrichment section:
   - the service's scene id (a bounded opaque string);
   - scene title;
   - studio name;
   - release date (`YYYY-MM-DD`, validated);
   - kind (scene, movie or VR);
   - performer names;
   - `performers_truncated`.
2. **Caps.** Each field is length- and count-capped with named `MAX_*` constants, in the same style as v1. Pick caps that cover ENR-2b's and ENR-3's parser caps (`MAX_TITLE`, `MAX_NAME` = 256, `max_performers`), so a parsed match always fits.
3. **Compatibility:**
   - version 1 blobs decode unchanged;
   - the decoder still refuses unknown versions and any trailing or extra bytes;
   - the plain layer stays canonical: one encoding per value, so encode-decode-encode gives the same bytes.
4. **The writer emits v1 when there's no enrichment section,** so existing blobs and their hashes don't churn.
5. **No pipeline wiring.** ENR-1 owns the callers and ENR-4/ENR-5 the mapping. Don't edit `crates/sources-feeds/**`.
6. **No migration.** The blob lives in V14's `torrent_claims`. If you find you need a schema change, stop and tell the runner.

## Tests
- A v1 golden blob, committed as bytes, still decodes to the same value.
- Round-trip property tests for v1 and v2, including every cap, at the cap and one over.
- Malformed v2 is refused, never half-decoded: a cut blob at every byte, bad UTF-8, a bad date, an unknown kind, over-cap counts.
- The `claims` fuzz target is extended to v2. Run it 10 minutes clean and state that in the PR.
- No performer id or other personal field can be represented in the type at all.

## Own
`crates/store/src/claims.rs`, `crates/sources/src/claims.rs`, `fuzz/fuzz_targets/claims.rs`, its seeds under `fuzz/seeds/claims/**`, and the decisions file `docs/decisions/2026-11-05-ENR-0.md` (use the UTC date the PR opens). If another file needs a one-line shared edit, list it in the PR and tell the runner.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the fuzz run is clean, `/security-review` is clean, and a fresh self-review is done.

Branch `slice/ENR-0`. The PR is titled "ENR-0: Claim encoding v2 with an optional adult-enrichment section". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round. Times in docs are UTC (§2.35).
