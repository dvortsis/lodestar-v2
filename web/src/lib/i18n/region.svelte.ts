// The region preferences (§10.12): the display time zone and the first day of the week, next to
// the language. One place holds the saved values; the shared date formatters and calendars read
// the accessors below, so changing a setting applies at once without a reload.
import { initLocale } from './locale.svelte';

export type FirstDay = 'auto' | 'monday' | 'saturday' | 'sunday';

export interface Region {
	language: string;
	timeZone: string;
	firstDay: FirstDay;
}

/** Per-viewer hint so the first paint follows the last saved value; the server stays authoritative. */
export const REGION_CACHE = 'lodestar.region';
const FIRST_DAYS: readonly string[] = ['auto', 'monday', 'saturday', 'sunday'];

let zone = $state('');
let first = $state<FirstDay>('auto');
/** Bumped by every applied value, so an older answer can't overwrite a newer one. */
let seq = 0;
export const regionSeq = () => seq;

const text = (v: unknown) => (typeof v === 'string' ? v : '');

/** Reads untrusted JSON (a cache entry or a settings response); unknown values mean "automatic". */
export function narrowRegion(raw: Record<string, unknown> | null | undefined): Region {
	const firstDay = text(raw?.firstDay ?? raw?.first_day_of_week);
	return {
		language: text(raw?.language),
		timeZone: text(raw?.timeZone ?? raw?.time_zone),
		firstDay: FIRST_DAYS.includes(firstDay) ? (firstDay as FirstDay) : 'auto'
	};
}

/** The saved time zone and first day (language goes through `initLocale`). */
export function setRegion(r: Pick<Region, 'timeZone' | 'firstDay'>): void {
	zone = knownZone(r.timeZone) ? r.timeZone : '';
	first = r.firstDay;
}

/** Applies all three settings. Call at start-up and whenever the Language & region screen saves. */
export function applyRegion(r: Region): void {
	seq++;
	setRegion(r);
	initLocale(r.language || null);
}

const knownZone = (name: string) => {
	if (!name) return false;
	try {
		new Intl.DateTimeFormat(undefined, { timeZone: name });
		return true;
	} catch {
		return false;
	}
};

const browserZone = Intl.DateTimeFormat().resolvedOptions().timeZone;

/** The IANA zone dates are shown in: the saved one, else the browser's. Reactive in runes. */
export function displayTimeZone(): string {
	return zone || browserZone;
}

/** The first-day setting as saved (`auto` follows the language; see `week.ts`). */
export const firstDaySetting = (): FirstDay => first;

export function readRegionHint(): Region {
	try {
		return narrowRegion(JSON.parse(localStorage.getItem(REGION_CACHE) ?? 'null'));
	} catch {
		return narrowRegion(null);
	}
}

export function writeRegionHint(r: Region): void {
	try {
		localStorage.setItem(REGION_CACHE, JSON.stringify(r));
	} catch {
		// Private mode or a full quota: the hint is optional.
	}
}

/** Saved values from the Language & region screen: apply them and refresh the hint. */
export function followSaved(r: Region): void {
	applyRegion(r);
	writeRegionHint(r);
}
