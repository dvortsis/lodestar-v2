# X-builder-group-case decisions (#1037)

Runner brief, 2026-11-21Z, after X-facet-group-case (#1033), which gave facet values a server display form (`FacetValue.label`, "RARBG" for `rarbg`) that the facet rail shows. The Builder's value options call `valueLabel(field, v, locale)` with no label (`web/src/lib/features/search/assist/BuilderRow.svelte`), so this slice checked whether a release group can show lower-cased there.

## Finding: no release-group value reaches the Builder's labelled options
- **`valueOpt` labels only a field's fixed list.** `valueOptions` in `BuilderRow.svelte` maps `spec.values`, and only when the field has one and isn't a `share`. Any other field is a free-text input, which shows the value as the user typed it, the same text the query carries.
- **`group` has no fixed list.** The search catalogue gives `TagKey::Group` the shape `(ValueType::Keyword, LIST, Vec::new())` (`crates/search/src/catalogue.rs`). `/search/fields` sends `values: None` for an empty list (`crates/api/src/dto/suggest.rs`), so the client sees `type: "keyword"` and no `values`.
- **The Builder takes nothing else.** It reads only `/search/fields`. The bar's `/suggest` dropdown (`assist/suggest.ts`) and the facet data don't feed its rows.

## Decision (runner)
- **Record only, no code change.** The Builder never shows a group in lower case that the user didn't type.
- **If `group` ever gains a fixed list,** the label has to come from the server alongside `values` in `SearchField`, under #1033's rule (S1-20: labels limited to ASCII re-casing). It's never re-derived on the client from a name.
- **No test is added for the free-text path** (runner): it's the Builder's existing behaviour for every keyword field.
