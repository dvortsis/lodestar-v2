import { DASH, DEFAULT_LOCALE, numberFormat, present, type MetricValue } from './shared';

const OPTS: Intl.NumberFormatOptions = { style: 'percent', maximumFractionDigits: 0 };

/**
 * Composition only. Input is the server's whole percent (0–100); dividing by 100 is a unit change
 * for `Intl`, not a computation. Compose the "82% video" label with the `dominant_share` message.
 */
export function formatShare(value: MetricValue, locale = DEFAULT_LOCALE): string {
	return present(value, (pct) =>
		pct < 0 ? DASH : numberFormat(locale, 'share', OPTS).format(pct / 100)
	);
}
