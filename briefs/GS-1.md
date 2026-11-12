# GS-1: siblings, identical payload and same file

**Runner brief, 2026-11-12 (UTC).** Opus, core, `/security-review` (the withheld rule; adult and hidden visibility). About 500 lines with tests. Run by the GS-0 session after #754 merged (53130ccc).
- **Branch:** `slice/GS-1` from fresh `origin/main`.
- **PR title:** `GS-1: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Goal
Close `TODO(M7)` in `crates/api/src/compare.rs` (~199). `GET /torrents/{hash}/siblings` today hard-codes `identical_payload` (NeedsPayloadSignature) and `same_file` (NeedsPieceRoots) as unavailable. Plan: `docs/plan/2026-11-05-v07-m7.md` (GS-1 row and line ~264).
- **Identical payload:** torrents with an equal `payload_sig`, found through the search index (GS-0's field; `SearchIndex::prefilter`). Then the stored file sizes confirm, so a signature collision never shows.
- **Same file:** torrents sharing a file's `pieces_root`, from the file index. This covers v2 and hybrid torrents only, and the response says so (an unavailable reason or a note for v1-only torrents). v1 per-file md5, sha1 and ed2k aren't used (plan Q18).
- **Both go through B13's `withheld` rule**, unchanged (`docs/decisions/2026-11-05-B13b.md`), and the same adult and hidden visibility as `other_versions`. Adult while Show adult is off is never shown; the request's own adult flag decides.
- **Cap and `truncated`** as for other versions. The request's `CancelToken` is checked before each search and each candidate's lookups.

## Requirements
1. **Partial while backfilling.** While GS-0's `payload_sig` backfill is incomplete, identical-payload results say they may be incomplete, using the same readiness signal GS-0's `sig:` uses. Never claim "none" when the index can't answer. Same for a file index that isn't populated (SB-6's population contract).
2. **No torrent without a signature matches another.** A torrent with no content files has no `payload_sig`, so it gets an unavailable reason, never a list of every other signature-less torrent.
3. **Bounded.** Candidate windows are capped. A torrent with very many files doesn't fan out into unbounded `pieces_root` lookups; cap the files considered and say so.
4. **No new text served** beyond reason codes. No change to which torrents are visible anywhere else.
5. **openapi.json and schema.d.ts** regenerated with the repo's tool if the DTO changes, never by hand.

## Owns
- `crates/api/src/compare.rs`
- `crates/api/src/dto/compare.rs`
- `crates/api/tests/siblings_payload.rs`
- `docs/decisions/2026-11-12-GS-1.md`

**Shared, and allowed small:** `web/src/lib/api/openapi.json` and `schema.d.ts` (regenerated only). Tell the runner **before** pushing if `crates/search/**` or the store needs a change.

## Tests
- Identical payload: clones with the same files are found; a same-signature, different-sizes collision isn't shown.
- Same file: hybrid and v2 torrents sharing a file are found; a v1-only torrent gets the unavailable or note reason.
- Withheld, adult and hidden rules: an adult clone is never shown with adult off; a withheld torrent stays withheld.
- Backfill incomplete: the response marks itself partial.
- Cap and `truncated`; cancellation.

No fixed sleeps. No tokio `test-util` or `testing` feature on the `lodestar` package.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-api`;
- `npm run check` in `web/` if the schema files changed.

Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus `/security-review` follows.
