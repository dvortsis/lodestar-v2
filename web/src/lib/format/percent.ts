import { DASH, DEFAULT_LOCALE, numberFormat, present, type MetricValue } from './shared';

const opts = (f: number): Intl.NumberFormatOptions => ({
	style: 'percent',
	minimumFractionDigits: f,
	maximumFractionDigits: f
});

/**
 * Percent of a 0–1 ratio from the server. `fractionDigits` is 1 for rates and 0 for distribution
 * shares. A non-zero value that would print as 0 shows `<0.1%` / `<1%`; a value below 1 that would
 * print as 100% shows `>99.9%` / `>99%`. Negative ratios are not valid here (use `formatDelta`).
 */
export function formatPercent(
	value: MetricValue,
	locale = DEFAULT_LOCALE,
	fractionDigits: 0 | 1 = 1
): string {
	return present(value, (n) => {
		if (n < 0) return DASH;
		const nf = numberFormat(locale, `percent${fractionDigits}`, opts(fractionDigits));
		const out = nf.format(n);
		if (n > 0 && out === nf.format(0)) {
			return `<${nf.format(fractionDigits === 1 ? 0.001 : 0.01)}`;
		}
		if (n < 1 && out === nf.format(1)) {
			return `>${nf.format(fractionDigits === 1 ? 0.999 : 0.99)}`;
		}
		return out;
	});
}
