# X-always-adult-detail-mask decisions

Slice X-always-adult-detail-mask (runner brief, 2026-11-12), the follow-up to #760 (ENR-12). In the binary, no `DrawerInterest` is installed, so the detail route had no always-adult mask, and ENR-12's "Identified as" section failed closed everywhere. Spec: `docs/decisions/2026-11-12-ENR-12.md`; architecture §20.9; X-source-adult's `AdultSources`.

## One source of truth: the manifests, through the sources listing
- **`ops::Source` gains `always_adult`**, served from each source's `Manifest::always_adult` (`crates/lodestar/src/ops.rs`). The detail route already reads this listing for names and privacy (`plugin_names`), and the host always serves it, drawer or not.
- **The catalogue's bitset is the pipeline's rule:** a source id's bit is set when its manifest says `always_adult`, which is what `AdultSources::of` computes over the registered manifests. `DrawerInterest`'s mask, when a host installs one, is that same `AdultSources` bitset. The route uses the union of the two, so a disagreement could only add bits, never drop one. The tests give the drawer exactly the manifests' mask and check that the answers match with and without it.

## Failing closed (brief requirement 2)
- **"Identified as" counts a finder as always adult** when its manifest says so, **or when it can't be confirmed not to be**:
  - a bit in the torrent's `sources` with no registered source;
  - a registered source whose plugin this build doesn't run, so its manifest isn't known.

  Any such finder hides the section.
- **`None` (no section at all)** only when the plugins couldn't be listed (`plugin_names` empty: no pipeline served, or its listing failed). Then nothing about any source is known.
- **Which case maps to which:**

  | Case | Result |
  |---|---|
  | Known not always-adult | counts for nothing |
  | Known always-adult | hides the section |
  | Unknown plugin, or unregistered bit | hides the section |
  | No listing | `None`: no section |

## ENR-7's adult section (brief requirement 3)
- **It had the same dependence:** "found by an always-adult source unless you said Not adult" read only the drawer's mask, so in the binary such a torrent that wasn't yet reclassified Adult got no adult section.
- **Fixed the same way, but with the known always-adult bits only.** For ENR-7, counting a finder as always adult *widens* (it makes the torrent adult, so the section shows and a lookup may be offered). So unknown finders don't count there: the safe direction is the opposite of "Identified as"'s.
  - It's still only for the admin with "Show adult content" on, never for a held, hidden or private torrent, and never after you said Not adult.
  - Drawer interest is still offered only when a host installs `DrawerInterest`.

## The two windows (brief requirement 4)
- **Before `adult_by_source` reclassifies a torrent** an always-adult source found, its stored type isn't Adult yet. The finder rule now hides "Identified as" anyway, drawer or not.
- **After you mark it Not adult,** the stored type is no longer Adult. The finder rule still hides "Identified as", which is stricter than ENR-1's gate (a Not adult correction undoes the source's mask there). ENR-12's visibility is never widened: Adult goes only through ENR-1's section.
- ENR-7's section, by contrast, honours Not adult as before.

## Nothing else changes
No new route and no new text served. The served `/sources` shape gains one boolean per source, `always_adult`, in `GET /sources` and `GET /sources/{id}`, which the admin already reads. `openapi.json` and `schema.d.ts` are regenerated with the repo's tools (`cargo run -p lodestar --bin openapi`, `npm run api:types`). Which torrents are visible doesn't change. The "Identified as" section can now appear where ENR-12's rules allow it, and ENR-7's where its own rules always meant it to.

## Files outside the brief's owns
- The brief's shared allowance: `crates/api/src/torrents.rs`, `crates/api/tests/identified.rs` and `crates/api/tests/adult_enrichment.rs`, plus the regenerated `openapi.json` and `schema.d.ts`.
- Test-only fixture lines, one `always_adult: false` each (runner-approved, 2026-11-12): `crates/api/tests/ops.rs` (two), `crates/api/tests/sources_spec.rs`, and `web/src/lib/features/sources/fixtures.ts`'s `source()` builder. They're needed because the field is required in the served shape.
- The six `/sources` goldens in `crates/api/tests/golden/` (`ops_sources`, `ops_source_get`, `ops_source_priority`, `ops_source_credentials`, `ops_source_consent_given`, `ops_source_egress_socks5`), re-blessed with `LODESTAR_BLESS=1` (runner-approved). Each only gains `"always_adult":false`.
