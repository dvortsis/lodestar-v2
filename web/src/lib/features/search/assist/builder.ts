// The Builder's model (§10.4): rows of field / operator / value over the server's AST. A row is
// read from a top-level clause, and a row's text goes to the server (`/parse`), which owns the
// syntax and the units; the Builder never prints or parses query text itself beyond joining
// `field:operator value`. Edits go back through `setAst`, so the bar's text is always the server's.
import type { Ast } from '$lib/state/search.svelte';
import { topLevel } from '../query/clauses';

import type { Field, Op } from './fields.svelte';

export type { Op };

export interface Row {
	field: string;
	op: Op;
	value: string;
}

/** The text the server parses for a row: `res:2160p`, `year:>=2020`, `discovered:<7d`. */
export function rowText(r: Row, type?: Field['type']): string {
	let value = r.value.trim();
	// Words in a text field are one phrase, not a clause and loose words.
	if ((type === undefined || type === 'text') && /\s/.test(value) && !value.startsWith('"'))
		value = `"${value}"`;
	return `${r.field}:${r.op === ':' ? '' : r.op}${value}`;
}

/**
 * The editable row for a clause, or null when the Builder can't show it as one (groups, ranges
 * with two bounds, sizes in bytes, dates, …): those stay as read-only rows with their label.
 */
export function rowOf(node: Ast, find?: (name: string) => Field | undefined): Row | null {
	switch (node.op) {
		case 'tag':
			return { field: node.key, op: ':', value: node.any.join(',') };
		case 'ext':
			return { field: 'ext', op: ':', value: node.any.join(',') };
		case 'word':
			return node.field === 'any' ? null : { field: node.field, op: ':', value: node.word };
		case 'range': {
			const field =
				node.field === 'season' || node.field === 'episode' ? node.field[0]! : node.field; // the AST says season, the query s
			if (find?.(field)?.type === 'size') return null; // bytes: a number the user never typed
			const { lo, hi } = node;
			if (lo !== null && hi !== null)
				return lo === hi ? { field, op: ':', value: String(lo) } : null;
			if (lo !== null) return { field, op: '>=', value: String(lo) };
			if (hi !== null) return { field, op: '<=', value: String(hi) };
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
