import { DASH, DEFAULT_LOCALE, numberFormat, present, splitUnit, type MetricValue } from './shared';

const MS_0: Intl.NumberFormatOptions = {
	style: 'unit',
	unit: 'millisecond',
	unitDisplay: 'short',
	maximumFractionDigits: 0
};
const MS_1: Intl.NumberFormatOptions = { ...MS_0, maximumFractionDigits: 1 };
const SEC: Intl.NumberFormatOptions = { ...MS_0, unit: 'second', maximumFractionDigits: 1 };

function pick(locale: string, ms: number): { fmt: Intl.NumberFormat; n: number } {
	if (ms < 1000 && Math.round(ms) < 1000) {
		return ms < 1
			? { fmt: numberFormat(locale, 'ms1', MS_1), n: ms }
			: { fmt: numberFormat(locale, 'ms0', MS_0), n: ms };
	}
	// From 1,000 ms; also when 999.5+ ms would otherwise print as "1,000 ms".
	return { fmt: numberFormat(locale, 'sec1', SEC), n: ms / 1000 };
}

/** Value and unit separately, for axis labels. Negative or unknown input gives "—". */
export function formatLatencyParts(
	ms: number | null | undefined,
	locale = DEFAULT_LOCALE
): { value: string; unit: string } {
	if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return { value: DASH, unit: '' };
	const { fmt, n } = pick(locale, ms);
	return splitUnit(fmt, n);
}

/** Input milliseconds: `0.8 ms`, `12 ms`, `1.4 sec` (zh `0.8毫秒`). Histogram percentiles are always ≈. */
export function formatLatency(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return present(value, (ms) => {
		if (ms < 0) return DASH;
		const { fmt, n } = pick(locale, ms);
		return fmt.format(n);
	});
}
