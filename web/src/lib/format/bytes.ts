import { DASH, DEFAULT_LOCALE, NBSP, numberFormat, present, type MetricValue } from './shared';

const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const;
const fixed = (d: number): Intl.NumberFormatOptions => ({
	minimumFractionDigits: d,
	maximumFractionDigits: d
});

const digitsFor = (unit: number, v: number) => (unit === 0 ? 0 : v < 10 ? 2 : v < 100 ? 1 : 0);

/** Digits for `v`, taken from its rounded value so 9.996 prints `10.0`, not `10.00`. */
function settle(unit: number, v: number): number {
	const d = digitsFor(unit, v);
	return Math.min(d, digitsFor(unit, Number(v.toFixed(d))));
}

/** Scale bytes to the largest IEC unit whose value is >= 1, stepping up when rounding reaches 1024. */
function scale(bytes: number): { v: number; unit: number; d: number } {
	let v = bytes;
	let unit = 0;
	while (unit < UNITS.length - 1 && v >= 1024) {
		v /= 1024;
		unit++;
	}
	let d = settle(unit, v);
	if (unit < UNITS.length - 1 && Number(v.toFixed(d)) >= 1024) {
		v /= 1024;
		unit++;
		d = settle(unit, v);
	}
	return { v, unit, d };
}

/** Number and unit symbol separately, for axis labels. Unknown or negative sizes give "—". */
export function formatBytesParts(
	bytes: number | null | undefined,
	locale = DEFAULT_LOCALE
): { value: string; unit: string } {
	if (typeof bytes !== 'number' || !Number.isFinite(bytes) || bytes < 0) {
		return { value: DASH, unit: '' };
	}
	const { v, unit, d } = scale(bytes);
	return {
		value: numberFormat(locale, `bytes${d}`, fixed(d)).format(v),
		unit: UNITS[unit] ?? 'PiB'
	};
}

/** IEC bytes with fixed decimals by magnitude: `512 B`, `38.2 MiB`, `4.37 GiB`, `1.00 MiB`. */
export function formatBytes(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return present(value, (n) => {
		if (n < 0) return DASH;
		const { value: v, unit } = formatBytesParts(n, locale);
		return `${v}${NBSP}${unit}`;
	});
}
