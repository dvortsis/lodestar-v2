import { DASH, DEFAULT_LOCALE, numberFormat, present, splitUnit, type MetricValue } from './shared';

type Unit = 'second' | 'minute' | 'hour' | 'day';
// Seconds per unit, and the value (in seconds) at which the next unit takes over.
const STEPS: { unit: Unit; per: number; upto: number }[] = [
	{ unit: 'second', per: 1, upto: 60 },
	{ unit: 'minute', per: 60, upto: 3600 },
	{ unit: 'hour', per: 3600, upto: 172800 },
	{ unit: 'day', per: 86400, upto: Infinity }
];

const digits = (v: number) => (v < 10 ? 1 : 0);

function pick(seconds: number): { unit: Unit; v: number } {
	for (const step of STEPS) {
		if (seconds >= step.upto) continue;
		const v = seconds / step.per;
		// Rounding must not reach the next unit ("60 sec"): fall through to it instead.
		if (Number(v.toFixed(digits(v))) * step.per < step.upto) return { unit: step.unit, v };
	}
	return { unit: 'day', v: seconds / 86400 };
}

function fmt(locale: string, unit: Unit, v: number): Intl.NumberFormat {
	const d = digits(v);
	return numberFormat(locale, `span-${unit}-${d}`, {
		style: 'unit',
		unit,
		unitDisplay: 'short',
		maximumFractionDigits: d
	});
}

/** Value and unit separately, for axis labels. Negative or unknown input gives "—". */
export function formatSpanParts(
	seconds: number | null | undefined,
	locale = DEFAULT_LOCALE
): { value: string; unit: string } {
	if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds < 0) {
		return { value: DASH, unit: '' };
	}
	const { unit, v } = pick(seconds);
	return splitUnit(fmt(locale, unit, v), v);
}

/** Durations from seconds: `45 sec`, `12 min`, `5.2 hr`, `3.1 days` (zh `45秒`, `5.2小时`). */
export function formatSpan(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return present(value, (s) => {
		if (s < 0) return DASH;
		const { unit, v } = pick(s);
		return fmt(locale, unit, v).format(v);
	});
}
