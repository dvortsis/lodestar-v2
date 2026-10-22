import { formatCompact } from './compact';
import { DASH, DEFAULT_LOCALE, present, type MetricValue } from './shared';

/**
 * `1.2K/h`, `0/h`. The suffix is the `rate_per_hour` / `rate_per_day` message (`{n}/h`, `{n}/d`);
 * this module cannot import the message layer, so the wrapping is done here with the same pattern.
 * When W4 lands, swap the template for the message call.
 */
export function formatRate(
	value: MetricValue,
	locale = DEFAULT_LOCALE,
	per: 'hour' | 'day' = 'hour'
): string {
	return present(value, (n) =>
		n < 0 ? DASH : `${formatCompact(n, locale)}/${per === 'hour' ? 'h' : 'd'}`
	);
}
