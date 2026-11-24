# ENR-terms-api: Terms in the API: non-redistributable identifications hidden from other apps, attribution in the detail and Sources APIs

**Runner brief, 2026-11-24 (UTC).** Core slice, Opus. /security-review before you ask for review. Target < 1,200 changed lines.
- **Branch:** `slice/ENR-terms-api` from fresh `origin/main` (or the branch the harness fixes).
- **PR title:** `ENR-terms-api: Terms in the API: non-redistributable identifications hidden from other apps, attribution in the detail and Sources APIs`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
ENR-terms (#962) enforced lookup services' terms in the identify host and `stored_title`, but left the detail API alone (`docs/decisions/2026-11-16-ENR-terms.md`, "API" and "Attribution"). Today `crates/api/src/torrents.rs` serves the Identified-as section and the provenance claim differences to API-key and feed-token callers (`Viewer::Public`), including answers from plugins whose terms say `redistribute: false` (TMDB, AniList, Open Library…). That breaks those services' terms.

## Read first
- `docs/decisions/2026-11-16-ENR-terms.md` (all of it), `docs/decisions/*-ENR-12.md`, ENR-11's decisions file (2026-11-24).
- `docs/architecture.md` §6 and §10.5; `docs/owner-decisions.md` on terms and third-party data.
- `crates/api/src/torrents.rs`, `crates/api/src/dto/torrents.rs`, the `Viewer` type and how `facets.rs` already uses `Viewer::Public`; `identify::may_redistribute` in `crates/pipeline/src/identify.rs`; the plugins' `Terms`/`Attribution`; the ops catalogue and the Sources API.
- `crates/api/tests/identified.rs`; `runner/standards.md`.

## What to build
1. **Hide from other apps:** for `Viewer::Public`, the detail API leaves out every Identified-as answer and every provenance claim difference that came from a plugin whose terms don't allow redistribution. Use `may_redistribute`, shared, not a copy. Lodestar's own UI (the signed-in viewer) still sees them. Check every other public route that can carry such an answer (Torznab, Stremio, export, `/search` fields, `/suggest`) and name in the PR which ones were already safe and why; fix any that aren't only after asking the runner if it's outside your owns.
2. **Attribution:** `Attribution { text, link, logo }` reaches the detail DTO beside each answer that needs it, and the Sources API through the ops catalogue. Never invent attribution; `None` stays absent. Links and logos are data from the plugin's own terms, not from the remote service.
3. Regenerate `web/src/lib/api/openapi.json` (Rust area) and any generated web types the build needs; no UI work (that's ENR-terms-ui).
4. Tests in `crates/api/tests/identified.rs`: a public viewer never sees a `redistribute: false` answer or claim, the UI viewer does, a redistributable one (Wikidata, MusicBrainz) shows to both, attribution present exactly when the terms carry it.
5. Refine your owns at launch: add the Sources/ops-catalogue files and the openapi you touch to your `runner/slices.yaml` entry (your own entry only), and run `cd tools/status-connector && npm test`.
6. Decisions file `docs/decisions/<date>-ENR-terms-api.md`.

## Owns
`crates/api/src/torrents.rs`, `crates/api/src/dto/torrents.rs`, `crates/api/tests/identified.rs`, the Sources/ops-catalogue API files you need (name them in your entry), `web/src/lib/api/openapi.json`, and your decisions file. Not crates/store or crates/pipeline: ENR-G is changing purge and ingest there now. Ask the runner before touching anything else.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; in `web/` `npm run check` and `npm run build` if openapi changed. No fixed sleeps. Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

Landing lane: if #1112 (X-escalate-full-table) or ENR-G enters the lane (both Rust), hold Rust pushes until it merges.

No AWS or cloud calls, no new accounts, no spending, no force-push.
