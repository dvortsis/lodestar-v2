# ENR-terms-api decisions

Slice ENR-terms-api (runner brief, 2026-11-24): lookup services' terms reach the API. A caller other than Lodestar's own UI never gets an identification answer or a claim that its service's terms keep inside Lodestar, and each answer and Sources row carries the credit its service asks for. Spec: architecture §6 and §10.5, `docs/decisions/2026-11-16-ENR-terms.md` ("API" and "Attribution"), `docs/decisions/2026-11-24-ENR-11.md`. The owner handed the runner's role to the owner's Claude Duet chat on 2026-11-24; that runner launched this slice, and the refinements below (above all the reader, under "Which claims the terms govern") are the runner's call under §2.19, flagged to the owner.

## Who is held to the terms
- **`Viewer::Public` (an API key, a feed token, or a handler reached with no access record) never gets a governed claim that its source's terms don't let leave Lodestar.** The admin session (the signed-in browser or the trusted proxy's user) is Lodestar's own UI and gets them all. Reason: ENR-terms, "`redistribute: false`: its answers stay in Lodestar's own UI".
- **Today the detail route is the admin session's only** (`auth::protected`): a key or token gets `401` before anything is read, as `crates/api/tests/provenance.rs` already pins. So the brief's "today `torrents.rs` serves them to API-key and feed-token callers" holds only for the `Viewer::Public` code path, which no mounted route reaches yet. The filter is built anyway, so the route stays within the terms the day it takes a key (its DTO already says "once these routes take one"), and `identified.rs` pins the `401` with no answer text for both a key and a token.

## Which claims the terms govern (`torrents::terms_closed`)
- **A claims row is governed when its source has a terms record** (a lookup plugin: everything it says is its service's data, a finder claim included) **or when the row is an identification answer** (a details section with a match confidence, ENR-10's rule) whatever its source. An answer with no record is under the strictest terms, as ENR-terms reads a missing record.
- **A governed row passes only when the record allows redistribution.** A finder with no record (a feed, the DHT) isn't a lookup plugin and keeps its claim; private sources keep their own rule (§18.7).
- **The reader is `Store::redistributes`, not a new one.** It's the reader ENR-11 gave search, Torznab and export (`Store::served_identifications`), held to `identify::may_redistribute` by a property test over every record (`crates/search/tests/enrichment.rs`). Using it keeps the detail view and search on one reader without making `lodestar-api` depend on `lodestar-pipeline` outside its tests; the unit tests write the record through `identify::record_terms` and check `may_redistribute` agrees.
- **Computed only for `Viewer::Public`, and fails closed:** the admin's detail reads no extra state. The result is a `TermsClosed`, which only `TermsClosed::read(viewer, …)` builds and which remembers the caller it was read for; one read for the admin and handed a public caller keeps every claim and answer from it, so a wiring mistake can't pass "nothing closed".

## What a public caller gets
- **Identified as:** an answer from a closed source is left out, unannounced, as a private source's is. When every answer is left out, the section is absent. The chosen mark is never moved to another answer (rv-760's rule, unchanged).
- **Provenance:** a closed source's name stays (it's a finder's name, which §18.7 already decides), its claim is null and `withheld` is set, the same shape as a public source's private claim. `Provenance.withheld`'s description now names terms too.

## Attribution
- **`SourceAttribution { link, logo?, text }`** is one DTO, in `dto/ops.rs`, used by both `Source.attribution` (`GET /sources`) and `Identification.attribution` (the detail). Both fields, and `logo`, are absent when there's nothing, never null. Reason: the brief's "`None` stays absent", and a client can't mistake null for an empty credit.
- **It comes from the plugin's own compiled terms, never from the service.** `catalogue::terms(id)` (sources-plugins) lists every plugin with a `fn terms(&self)`, by manifest, each entry the module's own `TERMS` const beside its own manifest (one `own_terms!(module)` per entry, so they can't be paired across plugins); a unit test fails if a plugin with terms isn't listed, or its `fn terms` returns anything but `&TERMS`. The host's `ops::source` reads the credit there, and the detail view takes it from the same Sources listing it already reads for names and classes (`PluginName`), so the two can't disagree.
- **The record isn't the source of the credit.** `Terms::record` holds no attribution, and the record decides only redistribution and retention. The credit is display text, read from the build, so a source whose plugin is gone from the build shows its answers (to the admin) with no credit.
- **Checked at build time:** every credit is short plain text with no control characters, its link `https://` with only printable ASCII that needs no quoting in HTML and a plain host (no user info, no port), and a logo, when there is one, an asset id (`[a-z0-9-]`), never an address. No plugin has a logo yet (TMDB's asset is still to come, `docs/decisions/2026-11-15-ENR-tmdb.md`).

## The other public routes (checked, none changed)
- **Search (`/search` hits and their fields), `/suggest`, `/facets`, Torznab and export** never read claims. Their titles and IDs come from the index and the record, which take identification answers only through `identify::stored_title` and `Store::served_identifications`, both terms-checked (ENR-terms, ENR-11). Torznab's `imdbid`, `tmdbid` and `tvdbid` only query that index. The `source` facet and a hit's `source` name finders only.
- **Feeds** serve the same index; `crates/api/tests/provenance.rs` pins that a feed names no source.
- **Stremio:** this build has no Stremio route.
- **`crates/integrations`** (download clients) and webhooks carry no claims.
- **Adult enrichment** is the admin's only (§20.9), so no public caller reaches it.

## Files outside the brief's first owns
- `crates/api/src/dto/ops.rs` (`SourceAttribution`, `Source.attribution`), `crates/api/src/ops.rs` (the re-export), `crates/lodestar/src/ops.rs` (`attribution`, one field in `source`, a unit test), `crates/sources-plugins/src/catalogue.rs` (`terms`, two tests): the Sources and ops-catalogue files the brief names, listed in the slice's `runner/slices.yaml` entry.
- One-line `attribution: None` additions to `Source` and `PluginName` literals in `crates/api/src/insights.rs`, `crates/api/tests/ops.rs` and `crates/api/tests/sources_spec.rs`, also added to the slice's owns.
- The regenerated `web/src/lib/api/openapi.json` and `schema.d.ts`. No UI change: rendering the credit is ENR-terms-ui.
