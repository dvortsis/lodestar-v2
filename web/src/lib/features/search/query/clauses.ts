// The active-filter strip's model (§10.4): one chip per structured top-level clause of the server's
// AST. Free text stays in the bar. Labels come from messages, with values through $lib/format only;
// the client never reads or prints query text itself (a removal goes back through the server).
import {
	formatBytes,
	formatCount,
	formatDate,
	formatScore,
	formatShare,
	formatSpan
} from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import type { Ast } from '$lib/state/search.svelte';

/** A label piece: plain text, or a value shown in mono (`full` is read instead of `mono`). */
export type Part = string | { mono: string; full?: string };

export interface Label {
	parts: Part[];
	/** The whole label as plain text: the accessible name and the announcement. */
	text: string;
}

export interface Clause {
	node: Ast;
	/** Position among the top-level clauses. */
	index: number;
	label: Label;
}

type Of<O extends Ast['op']> = Extract<Ast, { op: O }>;

/** Nested labels stop here, and a label longer than this is cut. */
const MAX_DEPTH = 3;
const MAX_CHARS = 80;
const ELLIPSIS = '…';
/** Hashes show this many characters; the accessible name keeps all 40. */
const HASH_CHARS = 12;

/** The top-level clauses: the `of` of a root `and`, else the root alone; `all` has none. */
export function topLevel(ast: Ast | null | undefined): Ast[] {
	if (!ast || ast.op === 'all') return [];
	return ast.op === 'and' ? ast.of : [ast];
}

/** Free text (any field): stays in the bar, gets no chip. */
export function isFreeText(node: Ast): boolean {
	switch (node.op) {
		case 'word':
		case 'phrase':
		case 'near':
		case 'prefix':
		case 'fuzzy':
			return node.field === 'any';
		case 'or':
			return node.of.every(isFreeText);
		default:
			return false;
	}
}

/** The clauses that get a chip, in query order. */
export function chipClauses(ast: Ast | null | undefined): Clause[] {
	return topLevel(ast).flatMap((node, index) =>
		isFreeText(node) ? [] : [{ node, index, label: label(node) }]
	);
}

function rebuild(of: Ast[]): Ast {
	const [only] = of;
	if (!only) return { op: 'all' };
	return of.length === 1 ? only : { op: 'and', of };
}

/** The root with top-level clause `index` removed: one left is unwrapped, none is `all`. */
export function rootWithout(ast: Ast, index: number): Ast {
	return rebuild(topLevel(ast).filter((_, i) => i !== index));
}

/** "Clear all": keeps the free text, drops every structured clause. */
export function withoutFilters(ast: Ast | null | undefined): Ast {
	return rebuild(topLevel(ast).filter(isFreeText));
}

// ---- labels ----------------------------------------------------------------------------------

const SLOT = '\u0001';

/** A message whose placeholders hold label parts: fills them in, keeping mono parts as parts. */
function compose<P extends object>(message: (p: P) => string, params: Record<keyof P, Part[]>) {
	const slots: Part[][] = [];
	const marked: Record<string, string> = {};
	for (const [key, parts] of Object.entries(params) as [string, Part[]][]) {
		marked[key] = `${SLOT}${slots.length}${SLOT}`;
		slots.push(parts);
	}
	return message(marked as P)
		.split(SLOT)
		.flatMap((piece, i) => (i % 2 ? (slots[Number(piece)] ?? []) : piece ? [piece] : []));
}

const plain = (parts: Part[]) =>
	parts.map((p) => (typeof p === 'string' ? p : (p.full ?? p.mono))).join('');

const mono = (text: string): Part => ({ mono: text });

/** Values joined as a short list ("Movie, TV"), in the current locale. */
function list(items: Part[][], type: 'unit' | 'disjunction' | 'conjunction' = 'unit'): Part[] {
	if (items.length === 1) return items[0] ?? [];
	// "Movie, TV" for values; "a and b" / "a or b" for clauses (short would print "&").
	const style = type === 'unit' ? 'short' : 'long';
	const fmt = new Intl.ListFormat(currentLocale(), { type, style });
	const out: Part[] = [];
	let next = 0;
	// Elements come back in order; literals are the locale's separators ("," "or").
	for (const piece of fmt.formatToParts(items.map((_, i) => String(i)))) {
		out.push(...(piece.type === 'element' ? (items[next++] ?? []) : [piece.value]));
	}
	return out;
}

const pair = (key: string, value: Part[]) => compose(m.search_filters_pair, { key: [key], value });

const NUM_KEY: Record<Of<'range'>['field'], () => string> = {
	size: m.search_filters_size,
	files: m.search_filters_files,
	largest: m.search_filters_largest,
	piece: m.search_filters_piece,
	year: m.search_filters_year,
	season: m.search_filters_season,
	episode: m.search_filters_episode,
	junk: m.junk
};

/** Byte fields go through formatBytes; counts and scores through theirs; year, season and
 *  episode are identifiers, shown as plain integers (no grouping: "2019", not "2,019"). */
function numValue(field: Of<'range'>['field'], n: number): string {
	const locale = currentLocale();
	switch (field) {
		case 'size':
		case 'largest':
		case 'piece':
			return formatBytes(n, locale);
		case 'files':
			return formatCount(n, locale);
		case 'junk':
			return formatScore(n, locale);
		default:
			return String(n);
	}
}

function bounds(
	key: string,
	lo: number | null,
	hi: number | null,
	show: (n: number) => string
): Part[] {
	if (lo !== null && hi !== null) {
		return lo === hi
			? pair(key, [show(lo)])
			: compose(m.search_filters_between, { key: [key], lo: [show(lo)], hi: [show(hi)] });
	}
	if (lo !== null) return compose(m.search_filters_at_least, { key: [key], value: [show(lo)] });
	if (hi !== null) return compose(m.search_filters_at_most, { key: [key], value: [show(hi)] });
	return compose(m.search_filters_any_value, { key: [key] });
}

type When = NonNullable<Of<'date'>['from']>;

/** Query dates are UTC days (docs/query-language.md "Dates"). */
const day = (at: number) => formatDate(at, currentLocale(), { timeZone: 'UTC' });
const span = (ago: number) => formatSpan(ago, currentLocale());
const end = (w: When) => ('ago' in w ? m.search_filters_ago({ span: span(w.ago) }) : day(w.at));

function dateLabel(node: Of<'date'>): Part[] {
	const key = m.discovered();
	const { from, to } = node;
	if (from && !to) {
		return 'ago' in from
			? [m.search_filters_in_last({ key, span: span(from.ago) })]
			: [m.search_filters_since({ key, date: day(from.at) })];
	}
	if (to && !from) {
		return 'ago' in to
			? [m.search_filters_more_than_ago({ key, span: span(to.ago) })]
			: [m.search_filters_until({ key, date: day(to.at) })];
	}
	if (from && to) {
		// One UTC day, as `discovered:2026-01-01` gives.
		if ('at' in from && 'at' in to && to.at - from.at === 86_399 && from.at % 86_400 === 0)
			return [m.search_filters_on({ key, date: day(from.at) })];
		return [m.search_filters_date_between({ key, from: end(from), to: end(to) })];
	}
	return compose(m.search_filters_any_value, { key: [key] });
}

const TEXT_KEY: Record<Of<'word'>['field'], (() => string) | undefined> = {
	any: undefined,
	name: m.raw_name,
	title: m.resolved_title,
	path: m.search_filters_path
};

/** Free-text syntax for a text clause, as the user would type it (the value only). */
function textValue(node: Of<'word' | 'phrase' | 'near' | 'prefix' | 'fuzzy'>): string {
	switch (node.op) {
		case 'word':
			return node.word;
		case 'phrase':
			return `"${node.text}"`;
		case 'near':
			return node.words.join(` NEAR/${node.slop} `);
		case 'prefix':
			return `${node.prefix}*`;
		case 'fuzzy':
			return `~${node.word}`;
	}
}

const FLAG_KEY: Record<string, () => string> = {
	private: m.search_filters_flag_private,
	v2: m.search_filters_flag_v2,
	hybrid: m.search_filters_flag_hybrid,
	imported: m.search_filters_flag_imported,
	backfill: m.backfill,
	files_partial: m.search_filters_flag_partial,
	ambiguous: m.search_filters_flag_ambiguous,
	hidden: m.hidden_by_filters,
	demoted: m.search_filters_flag_demoted,
	flagged: m.search_filters_flag_flagged
};

const TAG_KEY: Record<Of<'tag'>['key'], () => string> = {
	type: m.content_type,
	res: m.search_filters_res,
	hdr: m.search_filters_hdr,
	codec: m.search_filters_codec,
	audio: m.search_filters_audio,
	lang: m.search_filters_lang,
	group: m.search_filters_group,
	has: m.search_filters_has,
	resolved: m.search_filters_resolved,
	adult: m.adult_kind
};

// The glossary's `content_type` and `adult_kind` lists: labelled values, not codes.
const TYPE_VALUE: Record<string, () => string> = {
	movie: m.search_filters_type_movie,
	tv: m.search_filters_type_tv,
	music: m.search_filters_type_music,
	audiobook: m.search_filters_type_audiobook,
	ebook: m.search_filters_type_ebook,
	comic: m.search_filters_type_comic,
	software: m.search_filters_type_software,
	game: m.search_filters_type_game,
	images: m.search_filters_type_images,
	video: m.search_filters_type_video,
	other: m.search_filters_type_other,
	adult: m.search_filters_type_adult
};

const ADULT_VALUE: Record<string, () => string> = {
	scene: m.search_filters_adult_scene,
	movie: m.search_filters_adult_movie,
	photos: m.search_filters_adult_photos,
	comics: m.search_filters_adult_comics,
	game: m.search_filters_adult_game,
	vr: m.search_filters_adult_vr,
	audio: m.search_filters_adult_audio
};

/** `cat:` and `bytes:` kinds are the glossary's file categories. */
const CATEGORY: Record<string, () => string> = {
	video: m.video,
	audio: m.audio,
	image: m.image,
	document: m.document,
	ebook: m.ebook,
	archive: m.archive,
	disc: m.disc,
	software: m.software,
	subtitle: m.subtitle,
	other: m.other
};

function tagValue(key: Of<'tag'>['key'], value: string): Part[] {
	const named = key === 'type' ? TYPE_VALUE[value] : key === 'adult' ? ADULT_VALUE[value] : null;
	return [named ? named() : mono(value)];
}

function shareLabel(node: Of<'share'>): Part[] {
	const name = CATEGORY[node.cat]?.() ?? node.cat;
	const key = (node.by === 'count' ? m.search_filters_share_files : m.search_filters_share_bytes)({
		cat: name
	});
	const pct = (n: number) => formatShare(n, currentLocale());
	// Shares run 0–100: an open end is 0 or 100 (the server prints `cat:video>=80` the same way).
	const { lo, hi } = node;
	if (lo === hi) return bounds(key, lo, hi, pct);
	return bounds(key, lo > 0 ? lo : null, hi < 100 ? hi : null, pct);
}

/** Label parts for any node; nested clauses are labelled recursively up to MAX_DEPTH. */
function parts(node: Ast, depth: number): Part[] {
	if (depth > MAX_DEPTH) return [ELLIPSIS];
	const inner = (n: Ast) => parts(n, depth + 1);
	switch (node.op) {
		case 'all':
			return [m.search_filters_everything()];
		case 'and':
			return compose(m.search_filters_all_of, {
				clauses: list(node.of.map(inner), 'conjunction')
			});
		case 'or':
			return compose(m.search_filters_any_of, {
				clauses: list(node.of.map(inner), 'disjunction')
			});
		case 'not':
			return compose(m.search_filters_not, { clause: inner(node.of) });
		case 'file':
			return compose(m.search_filters_file, { clause: inner(node.of) });
		case 'word':
		case 'phrase':
		case 'near':
		case 'prefix':
		case 'fuzzy': {
			const key = TEXT_KEY[node.field];
			const value = [mono(textValue(node))];
			return key ? pair(key(), value) : value;
		}
		case 'range':
			return bounds(NUM_KEY[node.field](), node.lo, node.hi, (n) => numValue(node.field, n));
		case 'date':
			return dateLabel(node);
		case 'ext':
			return pair(
				m.search_filters_ext(),
				list(node.any.map((e) => [mono(`.${e.replace(/^\./, '')}`)]))
			);
		case 'flag':
			return pair(FLAG_KEY[node.name]?.() ?? node.name, [
				node.value ? m.search_filters_yes() : m.search_filters_no()
			]);
		case 'hash': {
			const short =
				node.hash.length > HASH_CHARS ? node.hash.slice(0, HASH_CHARS) + ELLIPSIS : node.hash;
			return pair(m.search_filters_hash(), [{ mono: short, full: node.hash }]);
		}
		case 'tag':
			return pair(TAG_KEY[node.key](), list(node.any.map((v) => tagValue(node.key, v))));
		case 'share':
			return shareLabel(node);
		default: {
			// Exhaustive: a new AST op fails type-checking here until it has a label.
			const unknown: never = node;
			void unknown;
			return [m.search_filters_unknown()];
		}
	}
}

/** Cuts the visible parts to MAX_CHARS; `text` keeps the whole label (and the full hash). */
function truncate(all: Part[]): Part[] {
	let left = MAX_CHARS;
	const out: Part[] = [];
	for (const p of all) {
		const shown = typeof p === 'string' ? p : p.mono;
		if (shown.length <= left) {
			out.push(p);
			left -= shown.length;
			continue;
		}
		const cut = shown.slice(0, Math.max(0, left)) + ELLIPSIS;
		out.push(typeof p === 'string' ? cut : { mono: cut, full: p.full ?? p.mono });
		break;
	}
	return out;
}

export function label(node: Ast): Label {
	const all = parts(node, 0);
	return { parts: truncate(all), text: plain(all) };
}
