// What the listbox shows: the server's `/suggest` items, then this browser's recent queries and the
// saved searches that fit what is typed. Pure, so the combobox's tests can feed it directly.
import type { components } from '$lib/api/schema';

export type SuggestItem = components['schemas']['SuggestItem'];
export type SuggestKind = SuggestItem['kind'] | 'recent' | 'saved';

export interface Option {
	id: string;
	kind: SuggestKind;
	/** What is shown. */
	text: string;
	/** Torrents it would find (server items only). */
	count?: number;
	/** `count` is an estimate (a title past 1,000 matches): shown with "≈". */
	approx?: boolean;
	/** The text it puts in, and the characters of the query it replaces (the whole query: Infinity). */
	insert: string;
	replace: { start: number; end: number };
	/** Recents, saved searches and "did you mean" are whole queries: accepting runs them. */
	run: boolean;
}

export const MAX_OPTIONS = 10;

export interface Saved {
	name: string;
	q: string;
}

const fits = (haystack: string, needle: string) =>
	haystack.toLowerCase().includes(needle.trim().toLowerCase());

export function buildOptions(
	text: string,
	items: readonly SuggestItem[],
	recent: readonly string[],
	saved: readonly Saved[]
): Option[] {
	const out: Option[] = items.map((i, n) => ({
		id: `s${n}`,
		kind: i.kind,
		text: i.text,
		count: i.count,
		approx: i.count_approx === true,
		insert: i.insert,
		replace: i.replace,
		run: i.kind === 'did_you_mean'
	}));
	const whole = { start: 0, end: Infinity };
	const typed = text.trim();
	// Whole-query lists show for an empty bar, and as prefix matches while typing.
	for (const [n, q] of recent.entries()) {
		if (q === typed || (typed && !q.toLowerCase().startsWith(typed.toLowerCase()))) continue;
		out.push({ id: `r${n}`, kind: 'recent', text: q, insert: q, replace: whole, run: true });
	}
	for (const [n, s] of saved.entries()) {
		if (typed && !fits(s.name, typed) && !fits(s.q, typed)) continue;
		out.push({ id: `v${n}`, kind: 'saved', text: s.name, insert: s.q, replace: whole, run: true });
	}
	return out.slice(0, MAX_OPTIONS);
}

/** The query with an option applied. Ranges are in characters (code points), not UTF-16 units. */
export function applyOption(text: string, o: Pick<Option, 'insert' | 'replace'>): string {
	const chars = Array.from(text);
	const start = Math.min(o.replace.start, chars.length);
	const end = Math.min(o.replace.end, chars.length);
	return chars.slice(0, start).join('') + o.insert + chars.slice(end).join('');
}

/** A caret in UTF-16 units → characters, for `/suggest`'s `pos`. */
export function charPos(text: string, utf16: number): number {
	return Array.from(text.slice(0, utf16)).length;
}
