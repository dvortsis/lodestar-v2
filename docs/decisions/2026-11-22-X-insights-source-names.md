# X-insights-source-names decisions (#1039)

Runner brief, 2026-11-21Z, from X-labels (#1023): "Insights by-source rows carry the plugin's display name from the server". Architecture §10.9; `docs/metrics.md` §6.8, §7.7; `docs/decisions/2026-11-21-X-labels.md` ("Two lazy passes": the plugin-name table the client carried for Insights).

## What the API sends
- **`segments.by_source` rows are a type of their own, `SourceYield`,** with `name` beside `key`, in `GET /insights/overview` (current and `previous`). The digest and the `insights` event read the same response, so they carry it too.
- **`DiscoverSource` gains `name`,** in `GET /insights/discover`'s `by_source`.
- **`name` is the plugin's display name**, from the same cached `ops.sources()` registry provenance names its sources from (`torrents::plugin_names`). A plugin this build doesn't run keeps its manifest id, as `ProvenanceSource.name` does. A store id with no source row stays `source_{id}`, for both `key` and `name`.
- **`key` is unchanged,** the plugin's manifest id, so ordering, links and the `source:` field still read it.
- **Names are filled after the response cache.** Insights responses are cached by window and hour. `name_sources` fills `name` on each request, so a registry read that failed, which falls back to the ids, is never kept in the cache.
- **Tested end to end** (`crates/api/tests/insights.rs`, runner's review). The test pipeline lists sources with display names, and can be made to fail its listing.
  - `by_source_rows_carry_the_plugins_names` checks the names on the overview's rows, current and previous, on Discover's rows and in the CSV. It checks the fallbacks too: `gone` shows its id, and `source_5` stays as it is.
  - `a_failed_listing_is_not_cached_with_the_response` gets ids while the listing fails. Then, in the same window and hour (a cache hit), it gets names once the listing has recovered. It waits for the registry's retry with a 20 s deadline, not a fixed sleep.
  - Both fail if the previous period's rows aren't named, and both fail if Discover's aren't.

## `by_ip_family` keeps its shape (runner, §2.26)
- `SegmentYield` was also `by_ip_family`'s row type. It stays as it was for that list, and the by-source list gets `SourceYield`, so a by-source row always has a name and `by_ip_family`'s JSON and CSV are byte-identical. A test pins both shapes (`by_ip_family_rows_carry_no_name`).
- **The response bytes are identical; the schema's description text isn't.** `SegmentYield.key`'s doc comment now reads "What it's counted by", since it's no longer a source's, so `openapi.json` describes the field differently.

## Names are compile-time text (runner, §2.26)
- **No clean-up helper.** A plugin's display name is its manifest's `name`, a `&'static str` compiled into the binary (`lodestar_sources::Manifest`), not input from outside. Svelte renders it as text, never as HTML. `ProvenanceSource.name` applies no clean-up either, and `crates/api/src/torrents.rs` is unchanged.
- **A test keeps it that way:** every built-in manifest's name is non-empty, has no edge spaces, is at most 64 characters, and has no control or invisible character (`every_manifest_name_is_short_plain_text`, `crates/sources-plugins/src/catalogue.rs`). A new manifest can't slip one in unseen.
- **If plugins ever take their names from outside the binary,** that's a new slice with its own security review.
- **The CSV export defuses a name like any other text** (formula and control characters; `text_is_defused_and_numbers_are_not` now covers the `name` column).

## The `fuzz (tpdb)` finding (runner)
- **What CI found:** the catalogue test made CI fuzz `sources-plugins`, and `fuzz (tpdb)` crashed on `{"data":{"$serde_json::private::RawValue":0,"id":316}}`.
- **The plugin's parse is correct.** `parse_answer` reads that key as an ordinary one and returns the match, id 316.
- **The harness's second reader was wrong.** The fuzz crate builds `serde_json` with its `raw_value` feature (`fuzz/Cargo.toml`), and `$serde_json::private::RawValue` is that feature's private marker key. `serde_json` then reads the object as a raw value and refuses the document, so the harness's "accepted what serde_json refuses" check fired.
- **The fix:**
  - `tpdb` and `stashdb` (the one other harness that compares a plugin through `serde_json::Value`) skip only that comparison when a key written without escapes starts `$serde_json::private::`. Every other check still runs. The check is in `fuzz/fuzz_targets/common/private_key.rs`.
  - An escaped key (`\u0024serde_json…`) isn't the token to `serde_json`, so it's still compared.
  - Two seeds keep both cases: `seeds/tpdb/private_raw_value_key.json` (the crash) and `…_escaped.json`. Run as inputs, the first crashes the old harness and passes the new one.
- **`fuzz/fuzz_targets/json.rs` has the same blind spot** (it fuzzes `sources-feeds`' shared reader). `{"$serde_json::private::RawValue":"[1]"}` makes its `serde_json` side read `[1]`. It's outside this slice; named to the runner.

## Who sees the names
- The Insights routes are `auth::protected`, and they already list every source's id. A name says nothing a viewer couldn't already see there, so §18.7's private-source rule is unchanged.

## The client
- **`web/src/lib/format/label-source-names.ts` is removed** (the plugin-name table the client carried for Insights), with its re-export in `labels.ts`.
- **Insights uses `sourceLabel(key, name)`,** the formatter "Found by" and the Source facet use: "DHT" as the glossary writes it, the importers' names, else the server's name, else the id. Callers:
  - `insights/overview/Segments.svelte` (the chart and the table);
  - `insights/stages/model.ts` (Discover's by-source table).
- **`labels.test.ts` drops its manifest-name check,** which read `crates/sources-plugins` to keep the client's table in step. There's no table left to keep in step; the server-side test above checks the names themselves.

## CSV export
- `segments.by_source`: `period,harvested,key,searchable,yield` → `period,harvested,key,name,searchable,yield`.
- `discover.by_source`: `period,dropped,key,rediscovered,reported` → `period,dropped,key,name,rediscovered,reported`.
- The columns stay in alphabetical order, as every panel's do. The goldens are re-blessed with `LODESTAR_BLESS=1`:
  - `insights_export_segments_by_source`, `insights_export_discover_by_source`;
  - `insights_overview_24h`, `_30d`, `_7d_compare`;
  - `insights_discover_24h`, `_7d_compare`.
  - The only change in them is the new `name`. The fixtures run without a pipeline, so each name there is its id.

## Release notes
- **Insights' source names come from the server.** The by-source rows on the overview and the Discover page show each plugin's own name.
- **Two CSV exports gain a column.** "Segments by source" and "Discover by source" now have a `name` column after `key`, which stays the plugin's id. A spreadsheet or script that reads columns by name keeps working. One that reads them by position (the fourth column, say) needs updating, because the columns after `key` move one place right.

## Sizes
gzip, main d6c83c56 → branch:

| Entry | Before | After | Limit before | Limit after |
|---|---|---|---|---|
| Insights stage pages (own JS + CSS) | 40,975 B | 40,655 B | 40.98 kB | 40.66 kB |

- The limit comes down to the measured size, rounded up to 0.01 kB, as the brief asks.
- Search route JS, the route table and total first load each moved −2 B (chunk hashes). No entry grew.
