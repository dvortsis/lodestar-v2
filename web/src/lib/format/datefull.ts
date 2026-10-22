import { dateFormat, DEFAULT_LOCALE, formatSeconds } from './shared';
import type { DateOptions } from './datetime';

export interface DateFullOptions extends DateOptions {
	/** Day and week buckets drop the hour, minute and zone name. Default `hour`. */
	bucket?: 'hour' | 'day' | 'week';
}

const DAY: Intl.DateTimeFormatOptions = {
	weekday: 'short',
	year: 'numeric',
	month: 'short',
	day: 'numeric'
};
const HOUR: Intl.DateTimeFormatOptions = {
	...DAY,
	hour: '2-digit',
	minute: '2-digit',
	hourCycle: 'h23',
	timeZoneName: 'short'
};

/** Chart tooltip headers: `Thu, Oct 22, 2026, 14:05 UTC`; day: `Thu, Oct 22, 2026`. Unix seconds. */
export function formatDateFull(
	seconds: number | null | undefined,
	locale = DEFAULT_LOCALE,
	opts: DateFullOptions = {}
): string {
	const coarse = opts.bucket === 'day' || opts.bucket === 'week';
	return formatSeconds(
		seconds,
		dateFormat(
			locale,
			coarse ? 'datefull-day' : 'datefull-hour',
			opts.timeZone,
			coarse ? DAY : HOUR
		)
	);
}
