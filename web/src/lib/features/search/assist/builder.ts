// The Builder's model (§10.4): rows of field / operator / value over the server's AST. A row is
// read from a top-level clause, and a row's text goes to the server (`/parse`), which owns the
// syntax and the units; the Builder never prints or parses query text itself beyond joining
// `field:operator value`. Edits go back through `setAst`, so the bar's text is always the server's.
import type { components } from '$lib/api/schema';
import type { Ast } from '$lib/state/search.svelte';
import { topLevel } from '../query/clauses';

type Schema = components['schemas'];
export type RowKind = 'tag' | 'text' | 'num' | 'date' | 'ext';

// Typed against the API's field enums: a field the server drops or renames fails the type check.
const TAG = [
	'type',
	'res',
	'hdr',
	'codec',
	'audio',
	'lang',
	'group',
	'has'
] as const satisfies readonly Schema['AstTagKey'][];
const TEXT = ['name', 'path', 'title'] as const satisfies readonly Schema['AstTextField'][];
const NUM = [
	'size',
	'files',
	'largest',
	'piece',
	'year',
	'season',
	'episode',
	'junk',
	'seeders',
	'leechers',
	'trackers',
	'dht'
] as const satisfies readonly Schema['AstNumField'][];
const DATE = ['discovered', 'seen'] as const satisfies readonly Schema['AstDateField'][];

export const FIELDS: { name: string; kind: RowKind }[] = [
	...TEXT.map((name) => ({ name, kind: 'text' as const })),
	...TAG.map((name) => ({ name, kind: 'tag' as const })),
	{ name: 'ext', kind: 'ext' },
	...NUM.map((name) => ({ name, kind: 'num' as const })),
	...DATE.map((name) => ({ name, kind: 'date' as const }))
];

export type Op = ':' | '>=' | '<=' | '>' | '<';
export const OPERATORS: Record<RowKind, Op[]> = {
	text: [':'],
	tag: [':'],
	ext: [':'],
	num: [':', '>=', '<=', '>', '<'],
	date: ['<', '>']
};

export interface Row {
	field: string;
	op: Op;
	value: string;
}

export const kindOf = (field: string): RowKind =>
	FIELDS.find((f) => f.name === field)?.kind ?? 'text';

/** The text the server parses for a row: `res:2160p`, `year:>=2020`, `discovered:<7d`. */
export function rowText(r: Row): string {
	const value = r.value.trim();
	return `${r.field}:${r.op === ':' ? '' : r.op}${value}`;
}

/**
 * The editable row for a clause, or null when the Builder can't show it as one (groups, ranges
 * with two bounds, sizes in bytes, dates, …): those stay as read-only rows with their label.
 */
export function rowOf(node: Ast): Row | null {
	switch (node.op) {
		case 'tag':
			return { field: node.key, op: ':', value: node.any.join(',') };
		case 'ext':
			return { field: 'ext', op: ':', value: node.any.join(',') };
		case 'word':
			return node.field === 'any' ? null : { field: node.field, op: ':', value: node.word };
		case 'range': {
			if (node.field === 'size') return null; // bytes: a number the user never typed
			const { lo, hi } = node;
			if (lo !== null && hi !== null)
				return lo === hi ? { field: node.field, op: ':', value: String(lo) } : null;
			if (lo !== null) return { field: node.field, op: '>=', value: String(lo) };
			if (hi !== null) return { field: node.field, op: '<=', value: String(hi) };
			return null;
		}
		default:
			return null;
	}
}

/** The root with the clause at `index` replaced (or, with null, `node` added at the end). */
export function withNode(ast: Ast | null, index: number | null, node: Ast): Ast {
	const tops = topLevel(ast);
	const next = index === null ? [...tops, node] : tops.map((n, i) => (i === index ? node : n));
	return next.length === 1 ? next[0]! : { op: 'and', of: next };
}
