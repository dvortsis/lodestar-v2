// What the screen's banners need on first paint (kept small: the rest of the rules load with the
// event list and the data kinds). Deadlines are the server's: 0 off, -1 until turned off, else Unix
// seconds on its clock.
import { m } from '$lib/paraglide/messages.js';
import type { components } from '$lib/api/schema';

type Catalog = components['schemas']['LogCatalog'];
type Kind = components['schemas']['LogKind'];

/** Whether a deadline is ahead of the server's clock. */
export const ahead = (until: number, now: number) => until === -1 || until > now;

/** The identifying kinds on now (the opt-ins have banners of their own). */
export const identifyingOn = (c: Catalog, now: number) =>
	c.kinds.filter((k) => k.identifying && !k.opt_in && ahead(k.until, now));

/** An opt-in, while it's on. */
export const optInOn = (c: Catalog, kind: string, now: number) =>
	c.kinds.find((k) => k.kind === kind && ahead(k.until, now));

/** When a set of kinds turns off: the soonest deadline, -1 if none ends, 0 if none is on. */
export function soonest(kinds: Pick<Kind, 'until'>[]): number {
	if (!kinds.length) return 0;
	const timed = kinds.map((k) => k.until).filter((u) => u !== -1);
	return timed.length ? Math.min(...timed) : -1;
}

/** The banners' time left ("This is on for about 42 min more."), or null once it's passed. */
export function timeLeft(until: number, now: number): string | null {
	if (until === -1) return m.logs_identifying({ time: m.logs_until_off() });
	const secs = Math.floor(until - now);
	if (until === 0 || secs <= 0) return null;
	return m.logs_identifying({
		time:
			secs >= 3600
				? m.logs_hours_left({ count: Math.round(secs / 3600) })
				: m.logs_minutes_left({ count: Math.max(1, Math.ceil(secs / 60)) })
	});
}
