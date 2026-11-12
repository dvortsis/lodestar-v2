# ENR-12: Detail API, "Identified as" section

**Runner brief, 2026-11-12 (UTC).** Opus, core work, `/security-review` (it serves stored third-party text to the browser).
- **Branch:** `slice/ENR-12` from fresh `origin/main`, at or after 2e6ae3eb (#750 ENR-10).
- **PR title:** `ENR-12: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Goal
Add an **Identified as** section to the torrent detail API, so ENR-13 can show it in the drawer (§6, §10.5). The section shows what ENR-10's identification harness stored for a torrent:
- the canonical title;
- the year;
- the ids (imdb, tmdb, tvdb, …);
- alternative titles;
- genres;
- the match confidence;
- how the title was resolved (`resolved_via`).

Read each plugin's stored row as an alternative, with its confidence. The title the record shows stays `stored_title`'s (ENR-10 decisions file, the ENR-12 line).

## Owns
- `crates/api/src/torrents.rs`
- `crates/api/src/dto/torrents.rs`
- `crates/api/tests/identified.rs` (new)
- `docs/decisions/2026-11-12-ENR-12.md`
- `web/src/lib/api/openapi.json` and `schema.d.ts`: regenerated with the repo's tool, never hand-merged.

**Shared, and allowed small:**
- the OpenAPI registration lines;
- `crates/sources/src/identify.rs` and `crates/pipeline/src/identify.rs`, for N6 and N7 below only. ENR-10 owned these; no other running slice holds them now.

Tell the runner **before** pushing if anything else needs to change, especially `crates/store/**` (a new read query belongs in the store's existing read API, if there is one).

## Read first
- `docs/decisions/2026-11-12-ENR-10.md`: what is stored, `ResolvedVia`, `with_identified`, and the notes to ENR-12.
- `docs/decisions/2026-11-06-ENR-9.md` and `-ENR-9b.md`: the v3 claim details, and how every details text is judged before storage.
- The existing detail sections in `crates/api/src/torrents.rs`, especially ENR-7's adult-enrichment section (#491's pattern): visibility, `private_source`, and how source names are withheld.
- `docs/architecture.md` §6 and §10.5; CLAUDE.md; `runner/standards.md`.

## Requirements
1. **Visibility is never wider than the detail view's.**
   - The section appears only where the torrent's detail is visible at all.
   - It never names a source the detail view hides. A `private_source` torrent, or a source the user can't see, shows no source or plugin names.
   - If an identification came from an Adult-class or Account-class source, follow ENR-7's rule for showing it. Keep the safer option where unsure, and say so in the decisions file.
2. **Stored text only.** Serve only judged, stored fields, with no new judging at read time.
   - Bound every list: alternative titles and genres to the stored caps, ids to the known kinds.
   - Bound every string as the DTO's other text fields are.
   - Unknown id kinds aren't invented; skip or label them per the existing id rules.
3. **No identification:** the section is absent, or `null`, consistently with the other optional sections. Document which in the OpenAPI.
4. **`resolved_via`:** list only the values `ResolvedVia::ALL` lists. `plugin` stays unlisted and unlabelled until a slice makes it fire (ENR-10, runner 2026-11-12). ENR-12 doesn't, so don't add `detail_via_plugin` or a `labels.ts` entry. A stored `plugin` row reads as an alternative, with its confidence.
5. **N6 (from rv-750):** clamp `lookup_timeout` in `IdentifySource::new` to a sane range. State the bounds and why in the decisions file.
6. **N7 (from rv-750):** a timed-out lookup never reaches `HostLimiter::record`. Make a timeout count, as the backoff (N3) expects, so a slow host backs off.
7. **Tests** (`crates/api/tests/identified.rs`, plus unit tests):
   - present and absent identification;
   - a `private_source` torrent shows no source names;
   - an Adult torrent's section is hidden when the detail is;
   - the list and string caps;
   - multiple plugin rows read as alternatives in a stable order;
   - an OpenAPI drift test;
   - N6's clamp, and N7: a timeout counts in the limiter.

   No fixed sleeps: paused tokio time, or condition waits with deadlines ≥ 10 s. Never enable tokio's `test-util` on the `lodestar` package.

## Not in scope
- The drawer UI (ENR-13).
- Indexing and querying identification (ENR-11).
- Wikidata (ENR-14).
- Listing `resolved:plugin`.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-api -p lodestar-pipeline -p lodestar-sources`, and the workspace if disk allows;
- the regenerated web files pass `npm run check` in `web/`.

The PR description names the shared edits and the visibility calls. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review with security items follows.
