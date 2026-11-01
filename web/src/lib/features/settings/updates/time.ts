// Updates' dates, built on Intl directly: importing `$lib/format` from a Settings screen splits the
// format chunk that other routes share and costs them size (as Security's date.ts, S-security).
import { displayTimeZone } from '$lib/i18n/locale.svelte';

const zone = () => displayTimeZone() || undefined;

function fmt(seconds: number, locale: string, o: Intl.DateTimeFormatOptions): string {
	const at = new Date(seconds * 1000);
	try {
		return at.toLocaleString(locale, { ...o, timeZone: zone() });
	} catch {
		return at.toLocaleString(locale, o);
	}
}

/** A release or history date: medium date style. */
export const dayOf = (seconds: number, locale: string) =>
	fmt(seconds, locale, { dateStyle: 'medium' });

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
	['day', 86_400],
	['hour', 3_600],
	['minute', 60]
];

/** "3 hours ago (Nov 1, 2026, 3:00 AM)": the relative time with its absolute one. */
export function whenOf(seconds: number, locale: string, now = Date.now() / 1000): string {
	const gap = seconds - now;
	const [unit, size] = UNITS.find(([, s]) => Math.abs(gap) >= s) ?? ['minute', 60];
	const relative = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' }).format(
		Math.round(gap / size),
		unit
	);
	return `${relative} (${fmt(seconds, locale, { dateStyle: 'medium', timeStyle: 'short' })})`;
}
