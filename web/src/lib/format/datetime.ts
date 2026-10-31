import { displayTimeZone } from '$lib/i18n/region.svelte';
import { dateFormat, DEFAULT_LOCALE, formatSeconds } from './shared';

export interface DateOptions {
	/** IANA time zone. Defaults to the display time zone setting (`displayTimeZone()`), else the browser's. */
	timeZone?: string;
}

/** Drawer, tables, tooltips: `Oct 22, 2026, 14:05` (zh `2026年10月22日 14:05`). Input Unix seconds. */
export function formatDateTime(
	seconds: number | null | undefined,
	locale = DEFAULT_LOCALE,
	opts: DateOptions = {}
): string {
	return formatSeconds(
		seconds,
		dateFormat(locale, 'datetime', opts.timeZone ?? displayTimeZone(), {
			dateStyle: 'medium',
			timeStyle: 'short',
			hourCycle: 'h23'
		})
	);
}
