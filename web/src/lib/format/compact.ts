import { DEFAULT_LOCALE, numberFormat, present, type MetricValue } from './shared';

const OPTS: Intl.NumberFormatOptions = {
	notation: 'compact',
	compactDisplay: 'short',
	maximumFractionDigits: 1
};

/** `1.2K`, `2.4M` (zh: `8.4万`). Only where metrics.md says so: KPIs, stage strip, pill, axis ticks. */
export function formatCompact(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return present(value, (n) => numberFormat(locale, 'compact', OPTS).format(n));
}
