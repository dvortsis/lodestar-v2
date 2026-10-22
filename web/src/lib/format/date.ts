import { dateFormat, DEFAULT_LOCALE, formatSeconds } from './shared';
import type { DateOptions } from './datetime';

/** Day-bucket axis ticks and date-only facts: `Oct 22, 2026`. Input Unix seconds. */
export function formatDate(
	seconds: number | null | undefined,
	locale = DEFAULT_LOCALE,
	opts: DateOptions = {}
): string {
	return formatSeconds(seconds, dateFormat(locale, 'date', opts.timeZone, { dateStyle: 'medium' }));
}
