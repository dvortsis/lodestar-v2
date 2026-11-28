// Zero-result rescue (§10.4 "No results", U8): a filter typed as plain words, such as `seeders>5` or
// `size > 4gb`, finds nothing because it was searched as text. This finds those fragments and writes
// the clause the user meant (`seeders:>5`). It only proposes: the caller checks the proposal with
// the server's parser, and nothing runs until the user clicks. The field list is the server's
// (`GET /search/fields`); this file holds no field names.
import type { components } from '$lib/api/schema';

// Type-only here: the Builder's field store (`fields.svelte.ts`) is not imported, so this chunk
// shares no code with the Builder and neither's size moves with the other's.
type Field = components['schemas']['SearchField'];
type Op = ':' | '>=' | '<=' | '>' | '<';
const OPS: Record<string, Op> = { eq: ':', gte: '>=', lte: '<=', gt: '>', lt: '<' };

export interface Correction {
	/** The whole query with every fragment rewritten. */
	text: string;
	/** The rewritten clauses, in order, for the button and the announcement. */
	clauses: string[];
}

// A bare word, a comparison, a value: `seeders>5`, `size > 4gb`, `year>=2020`. A `:` after the word
// is already a clause, so it never matches.
const FRAGMENT = /(^|\s)([A-Za-z][A-Za-z_]*)\s*(>=|<=|>|<|=)\s*([^\s"()|]+)/g;

export function correct(text: string, fields: readonly Field[]): Correction | null {
	const byName = new Map(
		fields.flatMap((f) => [f.name, ...f.aliases].map((n) => [n.toLowerCase(), f] as const))
	);
	const clauses: string[] = [];
	const out = text.replace(
		FRAGMENT,
		(whole, lead: string, name: string, op: string, value: string, at: number) => {
			const field = byName.get(name.toLowerCase());
			if (!field) return whole;
			// Inside a quoted phrase it is words, not a filter.
			if (((text.slice(0, at + lead.length).match(/"/g) ?? []).length & 1) === 1) return whole;
			const want: Op = op === '=' ? ':' : (op as Op);
			// A `share` field puts its comparison in the value, so a bare `cat>video` is not for it.
			if (field.type === 'share' || !field.operators.some((o) => OPS[o] === want)) return whole;
			const clause = `${name.toLowerCase()}:${want === ':' ? '' : want}${value}`;
			clauses.push(clause);
			return lead + clause;
		}
	);
	return clauses.length ? { text: out, clauses } : null;
}
