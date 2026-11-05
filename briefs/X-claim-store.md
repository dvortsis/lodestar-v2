# X-claim-store: store claimed trackers in their canonical, guarded form
**Model:** Opus (untrusted input; security). Then `/security-review` and a fresh Opus reviewer. Runner brief, 2026-11-05T20:53Z.

## Why
X-claim-trackers (#500, merged c98be2d6) added a shared tracker guard in `crates/integrations/src/net/tracker.rs`. `public_tracker_url(raw) -> Option<String>` returns the canonical form of the address it checked: a lowercase host made only of letters, digits, `-` and `.`, or an IP literal, with udp rebuilt around the host it judged. The pipeline doesn't use that output yet. `crates/pipeline/src/unverified.rs` `store_claims` (~line 207) keeps a claimed tracker only when `public_tracker_url(t) == Some(t)`. That has two problems:
1. **It drops valid claims.** `https://t.example:443/announce`, `UDP://T.Example:6969/a` and similar are refused because they aren't already canonical.
2. **It stores the raw text,** not the address that was checked.

The doc on `tracker_url` (~line 182-191) also still mentions `wss` and "text-level", which is stale.

## Do (branch `slice/X-claim-store` from `origin/main`)
1. In `store_claims`, map each tracker through `public_tracker_url` (`filter_map`). Store the canonical form, and drop duplicates after canonicalising. Keep any existing cap on the number of trackers, and apply it after the dedup. Keep the order deterministic.
2. Correct the `tracker_url` doc, or replace the function with the guard if they're now the same thing.
3. **Tests:**
   - a claim with `:443`, uppercase, or a udp host in mixed case or percent-encoded is kept in canonical form;
   - two spellings of one tracker are stored once;
   - every refused class is still refused: localhost, private, link-local, single-label, `*.local`, `,` or `;` in the host, `@`;
   - the stored form re-parses to itself (it's idempotent).

   Also check the `verified_claims` fuzz target. It still asserts that stored trackers are guarded and canonical; extend it if not. Run it for 10 minutes and say so in the PR.
4. **Existing stores.** Claims already stored in the old raw form stay as they are. Say in the decisions file whether a backfill is needed. The runner expects not: the old rule kept only text that was already canonical.
5. **Decisions file:** `docs/decisions/2026-11-05-X-claim-store.md`.

## Own
- `crates/pipeline/src/unverified.rs`: `store_claims`, `tracker_url` and their tests only.
- `fuzz/fuzz_targets/verified_claims.rs`.
- The decisions file and the `slices.yaml` entry.

**Don't touch:** `crates/pipeline/src/writer.rs` (RP-5 is running there), `crates/integrations/**` (the guard is done), `crates/sources-plugins/**`.

## Done when
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` (or at least `pipeline` and `integrations`; say which)
- the fuzz run
- `/security-review`

Then open the PR "X-claim-store: Store claimed trackers in their canonical, guarded form" with the label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Push once per round. No fixed sleeps.
