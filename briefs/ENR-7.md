# ENR-7: Detail API, the adult-enrichment section only when Show adult content is on, and the rate limit on drawer-opened lookups
**Model:** Opus (content-safety core). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T15:45Z. Ships in v0.5.2 or later (the runtime stays unwired until ENR-4/ENR-5).

## Why
ENR-1 (#480) built the adult-enrichment runtime; ENR-1b (#497, in review) closes its gate and race. ENR-7 is the API side: the drawer shows a match only when the viewer's "Show adult content" is on, and opening a drawer feeds `Interest::opened` without letting a script keep a third-party service busy. It's on the ENR-G path (ENR-7 → ENR-8 → ENR-G).

## Read first
- `docs/plan/2026-11-04-v06-rulepacks.md` §4.3 "ENR-7" (the spec) and Q2, Q4, Q11, Q15.
- `docs/decisions/2026-11-05-ENR-1.md`, in particular "What is sent and when" and "What isn't done here" (the `Interest::opened` rate limit is assigned to you).
- `docs/decisions/2026-11-04-ENR-0.md` (the stored section's fields); `crates/pipeline/src/adult_enrich.rs` (`Interest`); `crates/api/src/torrents.rs` and `crates/api/src/dto/torrents.rs`.
- Once #497 merges, its decisions file `docs/decisions/2026-11-05-ENR-1b.md`.

## Do (branch `slice/ENR-7` from `origin/main`)
1. **The section.** `GET /torrents/{id}` gains `adult_enrichment` (studio, scene title, release date, performers as Q4 and ENR-0 allow, the source and its scene link), read from the stored ENR-0 claim section.
   - Returned **only** when the request's Show adult content preference is on. Otherwise the field is **absent, not empty or null**, so the API never reveals that a match exists. The existing 404 `adult_hidden` stays; the absent field is defence in depth.
   - Never for a `Viewer::Public` request unless the same preference rules allow it; never from a `private_source` claim; a torrent held, hidden or tombstoned returns what it returns today, with no section.
   - Never an info hash, file name or anything that ENR-0 doesn't store. Link: only an `https` URL on the matched plugin's own manifest host; otherwise drop the link and keep the rest.
   - When more than one source matched, follow ENR-1's rule (first by source id) so the drawer and the kind pin agree.
2. **Interest at the API.** The handler hands the torrent id to an optional `Interest` (none until the host wires a plugin; ENR-4/ENR-5 and the host slice do that). Only when the preference is on and the item is the kind `adult_enrich::check` could pass (the gate still decides; never duplicate or weaken it).
   - **Rate limit:** bounded per client and overall (for example, a small token bucket per authenticated session or address, and a global cap), so opening drawers in a loop can't keep the service busy. A refused interest is dropped silently; the response is unchanged, so timing and body reveal nothing. Pick the numbers, record them in the decisions file as a runner-flagged §2.26 choice, and test both limits.
3. **OpenAPI:** regenerate `openapi.json` and `schema.d.ts` (`npm run api:types`), never hand-merged. #490 (SRC-11c, near merge) also edits `crates/api/src/torrents.rs` and the API types; whichever merges later brings `main` in and regenerates.
4. **Tests** in `crates/api/tests/adult_enrichment.rs`:
   - both preference states (section present vs field absent: assert the key is missing from the JSON);
   - Public viewer, `private_source` claim, held/hidden item, a link on a foreign host;
   - interest fires only with the preference on, and both rate limits hold;
   - no fixed sleeps; a clock you control for the bucket.

## Own
`crates/api/src/torrents.rs`, `crates/api/src/dto/torrents.rs`, `crates/api/tests/adult_enrichment.rs`, the regenerated `web/src/lib/api/{openapi.json,schema.d.ts}`, `docs/decisions/2026-11-05-ENR-7.md`.
**Shared, minimal:** the `Interest` handle's slot in the API state (`crates/api/src/lib.rs` or the router state), one line; say the exact hunk in the PR.
**Don't touch:** `crates/pipeline/**` (ENR-1b, RP-4), `crates/lodestar/**` host wiring (later slice), `web/src/lib/features/detail/**` (ENR-8).

## Done when
`cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `/security-review` on the diff. Open the PR "ENR-7: Detail API adult-enrichment section, only when Show adult content is on", label `full-ci`. Add the `runner/slices.yaml` placement only if no rule covers ENR-7. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head via the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round. No fixed sleeps.
