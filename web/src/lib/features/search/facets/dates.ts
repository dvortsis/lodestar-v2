// Typed From and To dates for Discovered (§10.4, UI review U15). The same rule as the brush
// (bins.ts `wholeDays`): a day is a whole UTC day. From is that day's first second, To its last,
// so From = To is that single day and an empty end is open. Pure: the rail does the AST edit.
import { wholeDays } from './bins';

const DAY_TEXT = /^(\d{4})-(\d{2})-(\d{2})$/;

/** A `YYYY-MM-DD` day as its UTC midnight in seconds; null when empty or not a real day. */
export function parseDay(text: string): number | null {
	const m = DAY_TEXT.exec(text.trim());
	if (!m) return null;
	const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
	const ms = Date.UTC(y, mo - 1, d);
	const back = new Date(ms);
	if (back.getUTCFullYear() !== y || back.getUTCMonth() !== mo - 1 || back.getUTCDate() !== d)
		return null;
	return ms / 1000;
}

/** The UTC day of a second count as `YYYY-MM-DD` (what a date input shows). */
export function dayText(seconds: number | null): string {
	return seconds === null ? '' : new Date(seconds * 1000).toISOString().slice(0, 10);
}

export type TypedDates =
	| { ok: true; range: { lo: number | null; hi: number | null } | null }
	| { ok: false; reason: 'bad-day' | 'order' };

/**
 * The inclusive range two typed days make. Both empty: no clause (`range` null). One empty: that
 * end is open. A text that isn't a day, or From after To, is not applied.
 */
export function rangeOfTyped(from: string, to: string): TypedDates {
	const lo = from.trim() === '' ? null : parseDay(from);
	const hi = to.trim() === '' ? null : parseDay(to);
	if ((from.trim() !== '' && lo === null) || (to.trim() !== '' && hi === null))
		return { ok: false, reason: 'bad-day' };
	if (lo !== null && hi !== null && lo > hi) return { ok: false, reason: 'order' };
	const range = wholeDays({ lo, hi });
	return { ok: true, range: range && (range.lo !== null || range.hi !== null) ? range : null };
}
