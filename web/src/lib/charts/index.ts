// Chart primitives (§10.11). Prefer importing a component by path (`$lib/charts/Kpi.svelte`); this barrel is for types and convenience. LineChart pulls uPlot in lazily either way.
export { default as Kpi } from './Kpi.svelte';
export { default as CategoryBar } from './CategoryBar.svelte';
export { default as HBarList } from './HBarList.svelte';
export { default as Histogram } from './Histogram.svelte';
export { default as Sparkline } from './Sparkline.svelte';
export { default as DataTableAlternative } from './DataTableAlternative.svelte';
export { default as LineChart } from './LineChart.svelte';
export type { LineSeries } from './lineOptions';
export type { BrushRange } from './brush';
export { GROUP_ORDER, GROUP_COLOR, type Group } from './shared';
