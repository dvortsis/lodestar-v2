import { DEFAULT_LOCALE, numberFormat, present, type MetricValue } from './shared';

const OPTS: Intl.NumberFormatOptions = { maximumFractionDigits: 0 };

/** Exact integers with grouping: `1,284`. Tables, tooltips, text equivalents. */
export function formatCount(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return present(value, (n) => numberFormat(locale, 'count', OPTS).format(n));
}

/** 0–100 integer scores (junk score). Same options as `formatCount`. */
export function formatScore(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return formatCount(value, locale);
}
