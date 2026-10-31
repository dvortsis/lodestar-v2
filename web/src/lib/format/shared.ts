// Shared plumbing for the formatters: value states (metrics.md §1) and cached Intl instances.
// Internal to this folder; components import the formatters, never this file.

/** U+2014 em dash: the display of an unknown value. */
export const DASH = '—';
/** U+00A0 no-break space, between a number and a unit symbol the formatter adds itself. */
export const NBSP = ' ';
/** U+2212 minus sign. */
export const MINUS = '−';
/** Glossary key of the note a panel shows once when any of its values are sampled. */
export const SAMPLED_NOTE_KEY = 'sampled_note';

export const DEFAULT_LOCALE = 'en';

/**
 * A value as the server sends it. `null`/`undefined` is unknown (rendered "—", never 0).
 * `{ value, approx: true }` is an estimate or a sampled value ("≈ 2.4M"); the panel reads `sampled`
 * from the response and shows the `sampled_note` message once.
 */
export type MetricValue =
	number | null | undefined | { value: number | null | undefined; approx?: boolean };

export function isNum(n: unknown): n is number {
	return typeof n === 'number' && Number.isFinite(n);
}

/** Apply the §1 value states around a renderer. The renderer only ever sees a finite number. */
export function present(input: MetricValue, render: (n: number) => string): string {
	let n: unknown = input;
	let approx = false;
	if (typeof input === 'object' && input !== null) {
		n = input.value;
		approx = input.approx === true;
	}
	if (!isNum(n)) return DASH;
	const out = render(n);
	if (out === DASH) return DASH;
	return approx ? `≈${NBSP}${out}` : out;
}

const numberFormats = new Map<string, Intl.NumberFormat>();
const dateFormats = new Map<string, Intl.DateTimeFormat>();
const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();

/** One `Intl.NumberFormat` per locale and `key`; the key must identify `options` uniquely. */
export function numberFormat(
	locale: string,
	key: string,
	options: Intl.NumberFormatOptions
): Intl.NumberFormat {
	const id = `${locale}|${key}`;
	let f = numberFormats.get(id);
	if (!f) {
		f = new Intl.NumberFormat(locale, options);
		numberFormats.set(id, f);
	}
	return f;
}

/** One `Intl.DateTimeFormat` per locale, time zone and `key`. No zone means the display time zone setting. */
export function dateFormat(
	locale: string,
	key: string,
	timeZone: string | undefined,
	options: Intl.DateTimeFormatOptions
): Intl.DateTimeFormat {
	// '' or a zone this browser doesn't know (a setting saved elsewhere) must not break rendering.
	// The display zone setting registers itself on load (i18n/locale.svelte.ts); importing it here
	// would move this module between chunks and cost routes size (decisions file).
	const zone =
		(timeZone ?? (globalThis as { lodestarZone?: () => string }).lodestarZone?.()) || undefined;
	const id = `${locale}|${zone ?? ''}|${key}`;
	let f = dateFormats.get(id);
	if (!f) {
		try {
			f = new Intl.DateTimeFormat(locale, { ...options, timeZone: zone });
		} catch {
			f = new Intl.DateTimeFormat(locale, options);
		}
		dateFormats.set(id, f);
	}
	return f;
}

export function relativeFormat(locale: string): Intl.RelativeTimeFormat {
	let f = relativeFormats.get(locale);
	if (!f) {
		f = new Intl.RelativeTimeFormat(locale, { numeric: 'auto', style: 'long' });
		relativeFormats.set(locale, f);
	}
	return f;
}

/** Format Unix seconds with `fmt`; "—" when the timestamp is unknown or not a valid date. */
export function formatSeconds(
	seconds: number | null | undefined,
	fmt: Intl.DateTimeFormat
): string {
	if (!isNum(seconds)) return DASH;
	const d = new Date(seconds * 1000);
	return Number.isNaN(d.getTime()) ? DASH : fmt.format(d);
}

/** `{value, unit}` from `formatToParts` of a unit-style NumberFormat, for axis labels. */
export function splitUnit(fmt: Intl.NumberFormat, n: number): { value: string; unit: string } {
	let value = '';
	let unit = '';
	for (const p of fmt.formatToParts(n)) {
		if (p.type === 'unit') unit += p.value;
		else value += p.value;
	}
	return { value: value.trim(), unit: unit.trim() };
}
