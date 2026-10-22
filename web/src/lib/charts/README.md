# lib/charts

Tiny in-house SVG chart primitives (CategoryBar, HBarList, Sparkline, Histogram; target ≤ 6 KB) and the lazily imported uPlot wrapper. Imports only from `format/` and `theme/` tokens.

Primitives: `Kpi`, `CategoryBar`, `HBarList`, `Histogram` (+ keyboard/pointer brush), `Sparkline`, `DataTableAlternative` and `LineChart` (uPlot loaded with a dynamic `import()` on first mount, rebuilt on theme change, destroyed on unmount). Import from `$lib/charts`. Every number comes from `$lib/format`; strings arrive as props. Showcase: `/_dev/ui/charts` (test-only).
