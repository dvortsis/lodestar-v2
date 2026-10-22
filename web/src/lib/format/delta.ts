import { DEFAULT_LOCALE, MINUS, numberFormat, present, type MetricValue } from './shared';

export type DeltaKind = 'count' | 'compact' | 'percent';

const OPTS: Record<string, Intl.NumberFormatOptions> = {
	count: { maximumFractionDigits: 0, signDisplay: 'exceptZero' },
	compact: {
		notation: 'compact',
		compactDisplay: 'short',
		maximumFractionDigits: 1,
		signDisplay: 'exceptZero'
	},
	percent0: { style: 'percent', maximumFractionDigits: 0, signDisplay: 'exceptZero' },
	percent1: {
		style: 'percent',
		minimumFractionDigits: 1,
		maximumFractionDigits: 1,
		signDisplay: 'exceptZero'
	}
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
	const key = kind === 'percent' ? `percent${fractionDigits}` : kind;
	return present(value, (n) =>
		numberFormat(locale, `delta-${key}`, OPTS[key] ?? {})
			.format(n)
			.replaceAll('-', MINUS)
	);
}
