# GS-2: grouping producer (`group_key`, `group_minhash`) and siblings' similar payloads

**Runner brief, 2026-11-12 (UTC).** Opus, core. About 800 lines with tests. GS-1 merged as #772 (88a632a0).
- **Branch:** `slice/GS-2` from fresh `origin/main`.
- **PR title:** `GS-2: Grouping producer: group_key and group_minhash; siblings' similar payloads`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-05-v07-m7.md`: lines 27, 48–58 (fields and budget), 136 and 265 (the GS-2 row and its paragraph), Q8–Q11 (lines 310–313), and the chain notes at 241–253.
- `docs/decisions/2026-11-12-GS-1.md`, `docs/decisions/2026-11-06-GS-gate.md`, `docs/decisions/2026-11-04-SB-6.md`, and GS-0's decisions file (the `payload_sig` producer and its backfill and population pattern).
- architecture §6 (Grouping/dedup), §8, §3; `runner/standards.md`.

## What to build
1. **`group_key`:** the fast-column copy of B13b's title identity, `TitleIdentityParts::key` (Q8). There is one identity rule; don't write a second one.
2. **`group_minhash`** per Q9:
   - "major" files are ≥ 1 % of the payload and ≥ 1 MiB;
   - only torrents with ≥ 3 major files get a signature (others rely on `payload_sig`);
   - 16 hashes in 4 bands of 4, stored as band terms;
   - similar when the estimated Jaccard is ≥ 0.8 on the stored sizes.
3. **The population contract,** as GS-0 did it: a new-document write, the backfill walk, restart mid-walk, and a readiness signal. Until the field is ready, consumers say the answer may be incomplete. Never claim "none" when the index can't answer.
4. **Siblings: a "similar payloads" group** in `crates/api/src/compare.rs`.
   - Candidates come by band term, are confirmed on the stored sizes, and go through the same `withheld` rule (B13) and adult/hidden visibility as the other groups.
   - Capped, with `truncated`, and the request's `CancelToken` checked as GS-1 does.
   - It's partial while the backfill runs.
5. **GS-1 follow-ups (in `compare.rs`, which you own now):**
   - endpoint tests for the `Missing` paths of the GS-1 groups (`needs_payload_signature`, `needs_piece_roots`); rv-772 O2;
   - the same tests for the new group.
6. **The budget:** run `size-report --torrents 50000` and put the numbers in the PR. After GS-0 the torrent index is 476.6 B/torrent, leaving 35.4 B under 512 for GS-2, RX-1 and ENR-11 together. If GS-2 alone would take most of that, apply Q10 (a): 2 bands instead of 4, or a higher major-file threshold. Bring the numbers to the runner before asking for review. `size-report --gate` is in `ci-ok`.
7. **openapi.json and schema.d.ts:** regenerate them with the repo's tools if the DTO changes, never by hand.

## Owns
- `crates/classify/src/group.rs` and `crates/classify/tests/group.rs`
- `crates/search/src/doc.rs` and `crates/search/src/population.rs`
- `crates/search/tests/grouping.rs`
- `crates/api/src/compare.rs`, `crates/api/src/dto/compare.rs` and `crates/api/tests/siblings_payload.rs`
- `docs/decisions/2026-11-12-GS-2.md`

**Shared, allowed small:**
- one `mod group;` line in `crates/classify/src/lib.rs`;
- the regenerated schema files.

Ask the runner before touching any other `crates/search/**` file or the store.

## Tests
- **Property tests:**
  - equal multisets of major sizes give equal signatures;
  - the order of files doesn't matter;
  - the false-positive rate on a synthetic corpus stays under a bound you state.
- **Below the threshold:** fewer than 3 major files gives no signature and an unavailable reason, never a match with every other signature-less torrent.
- **Population:** new writes; the backfill; restart mid-walk; partial before ready.
- **Siblings:**
  - found and confirmed;
  - an estimated-only match that fails the stored-size check is dropped;
  - withheld, adult and hidden rules; a forged candidate is dropped whatever the index says;
  - cap, `truncated` and cancellation.
- No fixed sleeps (waits are conditions with ≥ 10 s deadlines). No tokio `test-util` or `testing` feature on `lodestar`.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-classify -p lodestar-search -p lodestar-api`;
- `npm run check` in `web/` if the schema files changed.

Name your refinements in the PR description. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. If the runner keeps this session after that, carry on. If not, push your work in progress to the branch, open the PR as a draft, and report its state.
