// One formatter per metric (docs/metrics.md §3). Import from here or from the module directly;
// unused formatters are tree-shaken.
export { DASH, SAMPLED_NOTE_KEY, type MetricValue } from './shared';
export { formatCount, formatScore } from './count';
export { formatCompact } from './compact';
export { formatBytes, formatBytesParts } from './bytes';
export { formatPercent } from './percent';
export { formatShare } from './share';
export { formatRate } from './rate';
export { formatDelta, type DeltaKind } from './delta';
export { formatLatency, formatLatencyParts } from './latency';
export { formatSpan, formatSpanParts } from './span';
export { formatRelative, type RelativeOptions } from './relative';
export { formatDateTime, type DateOptions } from './datetime';
export { formatDateFull, type DateFullOptions } from './datefull';
export { formatDate } from './date';
export { sizeBinEdges, sizeBinLabel, RANGE_SEPARATOR, type SizeBinEdges } from './histogram';
