# ENR-terms: each lookup service's terms, enforced by the identify harness

Runner brief, 2026-11-16. Kind: core. Model: Opus. /security-review (it decides what leaves the install and what is kept). Branch `slice/ENR-terms`, PR titled `ENR-terms: <title>`. Follow `.claude/skills/steward/SKILL.md`; the runner merges.

## Why
The API lane is adding twelve lookup plugins (MusicBrainz, Open Library, TMDB, TVDB, AniList, Discogs, srrDB, predb, AniDB, IGDB, Steam, debrid). Their terms differ: some forbid bulk collection, TMDB forbids keeping data over 6 months and sublicensing, and several require attribution. The runner decided (2026-11-16, under §2.19/§2.69, stricter than the owner's Q12, flagged to the owner) that the harness enforces them, with strict defaults, before any lookup plugin is wired into the host. The decision and its reasons are in the comments on #948, #938 and #954.

## What to build
1. **A terms description per identify plugin, with strict defaults.** Prefer a provided trait method (for example `Identifier::terms() -> Terms` with a default impl) over new `Manifest` fields, so open and future plugin PRs that build `Manifest` literals keep compiling. `Terms` holds:
   - `background_walk: bool` (default false);
   - `redistribute: bool` (default false);
   - `max_retention` (default: refresh within 180 days; TMDB sets 6 months as a hard limit);
   - `attribution: Option<Attribution>` (text, link, optional logo asset id);
   - `adult_marker_required: bool` (default true): a result without the service's adult marker is refused. Plugins for services that don't mark adult content (MusicBrainz, Open Library) opt out explicitly, with a comment.
2. **Runtime enforcement in `lodestar_pipeline::identify`** (`docs/decisions/2026-11-12-ENR-10.md`):
   - the background walk runs only for plugins whose terms allow it. Every other plugin gets user interest (opened items) only, within its rate cap and the existing per-day cap;
   - claims carry their plugin's retention. The runtime refreshes a claim on the next user interest once it's older than the refresh age, and drops it before the hard limit even if no one opens the item. Dropping must be bounded work per tick, resumable across restarts, and must never touch claims from other sources;
   - a test per rule, plus a property test over random plugin terms, items and clocks (stepping back included).
3. **Redistribution.** Claims from a plugin with `redistribute: false` stay in Lodestar's own UI. Find every path that serves claims to other apps (Torznab, export, any API for other apps; check `crates/integrations`, the export CLI and `crates/api`) and filter them there. Test each path.
4. **Attribution data** reaches the API the detail drawer and the Sources page read, so a UI slice can render it. The UI itself is a separate Sonnet slice (`ENR-terms-ui`), not this one.
5. **Set the terms for the plugins already on main** (Wikidata: background allowed, CC0; Open Library, ENR-ol: user interest only, no attribution required beyond a link if its terms ask). TMDB, MusicBrainz, AniList, TVDB and Discogs are still open PRs from the API lane; don't edit their files. Instead, list in your decisions file the `terms()` each one needs, and the runner relays it to the lane.

## Bounds
- Never loosen the per-day cap, the eligibility gate, consent, or what may be sent (ENR-10 Q5).
- No new dependencies without asking the runner.
- Owns: decide the exact files once you've read the code, and send the runner the list before writing. Expected: `crates/sources/src/identify.rs` (or a new `terms.rs` next to it), `crates/pipeline/src/identify.rs` and its tests, the claim-serving paths for redistribution, `docs/decisions/2026-11-16-ENR-terms.md`. Ask before touching the store schema (on-disk format) or `crates/api/src/torrents.rs`.
- Add your own `runner/slices.yaml` entry (one line, flow style, like the ENR-* entries) and a placement rule in `runner/dashboard.yaml` if none covers it. Nothing else in those files.
- Size ≲ 1,500 changed lines; split if bigger (tell the runner).
- No fixed sleeps; condition waits with ≥ 10 s deadlines. No 12-digit numbers except 111122223333 and 999988887777.

## Done
fmt, clippy -D warnings (Linux and `--target x86_64-pc-windows-gnu`), the touched crates' tests, status-connector `npm test`; then message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready <sha>".
