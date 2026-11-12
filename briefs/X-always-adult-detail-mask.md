# X-always-adult-detail-mask: the always-adult mask for the detail route in production

**Runner brief, 2026-11-12 (UTC).** Opus, core work, `/security-review` (content safety: the adult mask on stored third-party text).
- **Branch:** `slice/X-always-adult-detail-mask` from fresh `origin/main`, at or after 413dfe25 (#760 ENR-12).
- **PR title:** `X-always-adult-detail-mask: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.
- **Size:** small, about 300 lines with tests.
- **Must land before ENR-13 ships the drawer.**

## Goal
In the binary today, `identified_section` (`crates/api/src/torrents.rs`) always gets `always_adult: None`. The host installs no `DrawerInterest`, because no identify plugin is wired yet (ENR-14's wiring waits for the External consent gate). So the "Identified as" section never shows in production. It fails closed, which is safe, but useless.

Give the detail route the registered always-adult sources' bitset from somewhere the host always sets. Carry it on `ops::Source` (the catalogue the detail route already reads), from each source's `Manifest::always_adult`. Then the mask works whether or not a drawer plugin is wired.

## Owns
- `crates/api/src/dto/ops.rs`
- `crates/lodestar/src/ops.rs`
- `docs/decisions/2026-11-12-X-always-adult-detail-mask.md`

**Shared, and allowed small:**
- `crates/api/src/torrents.rs`: reading the bitset in the detail route and passing it to `identified_section` (and to ENR-7's section, if item 3 applies).
- `crates/api/tests/identified.rs` and `crates/api/tests/adult_enrichment.rs`: tests.
- `web/src/lib/api/openapi.json` and `schema.d.ts`: only if a DTO's served shape changes. Regenerate them with the repo's tool, never by hand.

Tell the runner **before** pushing if anything else needs to change, especially `crates/store/**`, `crates/sources/**` or `services.rs`.

## Requirements
1. **One source of truth.** Derive the always-adult bitset from the registered sources' manifests (`Manifest::always_adult`, `AdultSources`).
   - Use the same derivation as `DrawerInterest::new`'s argument, so the two can never disagree. Where `DrawerInterest` is installed, assert or test that the two are equal.
2. **It still fails closed.**
   - If the catalogue can't say (a source registered with no manifest, or a source id beyond the bitset), treat it as always-adult, or show no section. Never show one.
   - `None` stays possible only where truly unknown. Say which case maps to which in the decisions file.
3. **Check ENR-7's section for the same gap** (the adult-enrichment section, #491's pattern):
   - If it also depends on `DrawerInterest` for the always-adult mask, apply the same fix.
   - If not, say why in the decisions file.
4. **Cover two windows:**
   - before `adult_by_source` reclassification has run;
   - a torrent the user marked Not adult.

   The always-adult finder rule must still hide the "Identified as" section where ENR-12's rules say to. Don't widen anything ENR-12's visibility rules hide.
5. **No new route, no new text served,** and no change to which torrents are visible, except that the section can now appear where ENR-12's rules allow it.

## Tests
- In the binary's configuration (no `DrawerInterest` layer), a non-adult, public torrent with an identification shows the section.
- A torrent found by an always-adult source shows no section. Check before reclassification and after "Not adult".
- An unknown or unregistered source bit fails closed.
- The bitset equals `DrawerInterest`'s where both exist.
- ENR-7's section: the same cases, if item 3 applies.

No fixed sleeps. No tokio `test-util` or `testing` feature on the `lodestar` package.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-api -p lodestar`;
- `npm run check` in `web/` if the web files changed.

The PR description names the shared edits. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus `/security-review` follows.
