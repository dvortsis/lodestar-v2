import { DASH, DEFAULT_LOCALE, MINUS, numberFormat, present, type MetricValue } from './shared';

export type DeltaKind = 'count' | 'compact' | 'percent';

const EXCEPT_ZERO = 'exceptZero';
const OPTS: Record<DeltaKind, Intl.NumberFormatOptions> = {
	count: { maximumFractionDigits: 0, signDisplay: EXCEPT_ZERO },
	compact: {
		notation: 'compact',
		compactDisplay: 'short',
		maximumFractionDigits: 1,
		signDisplay: EXCEPT_ZERO
	},
	percent: { style: 'percent', signDisplay: EXCEPT_ZERO }
};

/**
 * A signed change: `+1,240`, `−3`, `0`; percent (a ratio): `−3.2%`, `0.0%`. Uses the metric's own
 * number options plus `signDisplay: 'exceptZero'` and replaces the hyphen-minus with U+2212. A value
 * that rounds to zero prints unsigned. Pair with an icon and the period text, never colour alone.
 */
export function formatDelta(
	value: MetricValue,
	locale = DEFAULT_LOCALE,
	kind: DeltaKind = 'count',
	fractionDigits: 0 | 1 = 1
): string {
	const base = OPTS[kind];
	if (!base) return DASH; // an unknown kind from untyped callers: never throw
	const opts =
		kind === 'percent'
			? { ...base, minimumFractionDigits: fractionDigits, maximumFractionDigits: fractionDigits }
			: base;
	const key = kind === 'percent' ? `percent${fractionDigits}` : kind;
	return present(value, (n) =>
		numberFormat(locale, `delta-${key}`, opts).format(n).replaceAll('-', MINUS)
	);
}
