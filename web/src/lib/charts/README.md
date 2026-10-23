# lib/charts

Tiny in-house chart primitives (CategoryBar, HBarList, Sparkline, Histogram, Kpi, DataTableAlternative; the SVG/HTML set is under the 6 KB gzipped target) and the lazily imported uPlot `LineChart`. Imports only from `format/`, `i18n` (locale) and `theme/` tokens.

- **Imports:** import a component by path (`$lib/charts/Histogram.svelte`) so only what a route uses is bundled. `$lib/charts` (the barrel) is for types and convenience. uPlot and its CSS load with a dynamic `import()` on the first `LineChart` mount and never join a route's first-load chunk.
- **Numbers and text:** every number is formatted through `$lib/format`; null is "—", estimates "≈". Visible text and accessible names arrive as props.
- **Every chart has a table.** Pair each chart with a `DataTableAlternative` built from the same rows (pre-formatted strings), shown by the panel's "View as table" toggle (`visible`) and otherwise exposed only to assistive tech (`sr-only`). Give the chart a one-sentence `label`/`summary`. See `src/dev-routes/ui/charts` for Histogram, CategoryBar and LineChart each paired with its table.
- **Histogram brush:** `onbrush` fires on every change of the range (cheap, local feedback); `oncommit` fires when the range is settled (pointer release, key press, clear) and is the place to query.
- **LineChart:** rebuilds on theme or option changes, `setData` on new samples; `bucket` (`hour|day|week`) picks the tick and tooltip date format; `integers` (default) keeps count axes to whole steps.
- Showcase: `/dev/ui/charts` (test-only; `src/dev-routes/ui/charts`).
