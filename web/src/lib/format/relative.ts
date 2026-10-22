import { DASH, DEFAULT_LOCALE, isNum, relativeFormat } from './shared';

const UNITS: { unit: Intl.RelativeTimeFormatUnit; seconds: number }[] = [
	{ unit: 'year', seconds: 365 * 86400 },
	{ unit: 'month', seconds: 30 * 86400 },
	{ unit: 'week', seconds: 7 * 86400 },
	{ unit: 'day', seconds: 86400 },
	{ unit: 'hour', seconds: 3600 },
	{ unit: 'minute', seconds: 60 },
	{ unit: 'second', seconds: 1 }
];

// Fallback for the `just_now` message until W4 supplies it; callers pass the translated string.
const JUST_NOW: Record<string, string> = { en: 'just now', zh: '刚刚' };

export interface RelativeOptions {
	/** Current time in Unix seconds. Defaults to the clock; pass it to re-render once a minute. */
	now?: number;
	/** The translated `just_now` message, shown when |Δ| < 45 s. */
	justNow?: string;
}

/**
 * Rows only: `3 hours ago`, `yesterday`, `in 5 minutes`. Input is Unix seconds. The unit is the
 * largest of second…year with |Δ| ≥ 1 (month = 30 d, year = 365 d), truncated toward zero. Always
 * paired with `formatDateTime` in a tooltip.
 */
export function formatRelative(
	seconds: number | null | undefined,
	locale = DEFAULT_LOCALE,
	opts: RelativeOptions = {}
): string {
	if (!isNum(seconds)) return DASH;
	const now = opts.now ?? Date.now() / 1000;
	if (!isNum(now)) return DASH;
	const delta = seconds - now;
	if (Math.abs(delta) < 45) {
		return opts.justNow ?? JUST_NOW[locale] ?? JUST_NOW[locale.split('-')[0] ?? ''] ?? 'just now';
	}
	const step = UNITS.find((u) => Math.abs(delta) >= u.seconds) ?? UNITS[UNITS.length - 1];
	return relativeFormat(locale).format(Math.trunc(delta / step!.seconds), step!.unit);
}
