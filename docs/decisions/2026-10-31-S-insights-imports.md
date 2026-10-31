# S-insights-imports decisions: the crawled/imported split on the Insights screens

Binds the fields IMP-5 added (`docs/decisions/2026-10-31-IMP-5.md`, "What the screens should show"). No number is computed in the client.

- **Searchable torrents:** the total, then "Crawled" (`kpi.searchable_crawled`) and "Imported" (`kpi.imported`). Δ isn't split; when an `import_finished` marker is in the range and a Δ is shown, a note says "Includes an import".
- **New searchable:** the crawled count stays the headline and Δ. "+ N imported" (`new_searchable_imported`, `formatCount`) shows under it only when N > 0.
- **Zero rows:** nothing new is shown for zero imports ("+ 0 imported" and the Index "Imported" metric are omitted). The existing "Imported:" line under Searchable torrents is unchanged (it already showed, including 0), so a zero-import install differs only by the new "Crawled:" line.
- **Movement chart:** imports are **not drawn** (`trend[].searchable_imported` unused). One import is thousands to millions of records in minutes; on the same axis it would flatten the crawl line, and a second axis is banned (§10.9.3). The import's size is told by its markers and the KPI line.
- **Markers:** import annotations are text in the existing annotations list (overview) and in an "Imports" list under the Index charts. Each is a tab stop with an accessible sentence: "Import started", "Import finished: N new, M already known", "Import removed: N deleted". A null count shows as a dash.
- **Index stage:** "Imported" (`new_searchable_imported`) beside "New searchable", shown only above zero. `IndexResponse` carries no annotations, so the page fetches `GET /insights/events` for its range after it renders (best-effort; a failure shows no markers). The Index trend charts are unchanged.
- **Mock:** the e2e harness answers `getInsightsEvents` with no events by default.
- **Sizes:** first load +11 B (total 85,720 → 85,731 B, within its limit); Insights stage pages +915 B gzipped (36,058 → 36,973 of 39,700). No limit changed.
- **Truncation:** when `EventsResponse.truncated` is true, import markers past the 1,000-annotation cap aren't shown on the Index page.
