// The Source facet as AST edits: a click adds or removes a plugin id in the query's first positive
// top-level `source:` clause. Include only (the facet offers no exclusion). Follows the facet
// rail's rule (`facets/toggle.ts`): a second positive clause is an AND with the first and a
// negated one is an exclusion, so folding them into one list would change what the query means.
// Those are left as typed, and their values are `fixedSources` (disabled in the rail).
import type { Ast } from '$lib/state/search.svelte';

type SourceNode = Extract<Ast, { op: 'source' }>;
const isSource = (n: Ast): n is SourceNode => n.op === 'source';
const conjuncts = (ast: Ast | null | undefined): Ast[] =>
	!ast || ast.op === 'all' ? [] : ast.op === 'and' ? ast.of : [ast];
const rebuild = (of: Ast[]): Ast => {
	const [only] = of;
	return !only ? { op: 'all' } : of.length === 1 ? only : { op: 'and', of };
};

/** The one clause a click rewrites: the first positive top-level `source:`. */
const editable = (ast: Ast | null | undefined): SourceNode | undefined =>
	conjuncts(ast).find(isSource);

/** The plugin ids the editable clause selects. */
export function selectedSources(ast: Ast | null | undefined): string[] {
	return editable(ast)?.any ?? [];
}

/** Values the query sets in clauses a click can't rewrite (a second `source:`, `-source:`). */
export function fixedSources(ast: Ast | null | undefined): Set<string> {
	const first = editable(ast);
	const out = new Set<string>();
	const walk = (n: Ast) => {
		if (isSource(n)) n.any.forEach((v) => out.add(v));
		else if (n.op === 'and' || n.op === 'or') n.of.forEach(walk);
		else if (n.op === 'not') walk(n.of);
	};
	for (const c of conjuncts(ast)) if (c !== first) walk(c);
	return out;
}

/** Adds `id` to the editable `source:` clause, or removes it when it is already there. */
export function toggleSource(ast: Ast | null | undefined, id: string): Ast {
	const all = conjuncts(ast);
	const first = editable(ast);
	const current = first?.any ?? [];
	const any = current.includes(id) ? current.filter((v) => v !== id) : [...current, id];
	if (!first) {
		const clause: Ast = { op: 'source', any };
		return rebuild([...all, clause]);
	}
	const clause: Ast = { op: 'source', any };
	return rebuild(
		any.length ? all.map((n) => (n === first ? clause : n)) : all.filter((n) => n !== first)
	);
}

/** Removes the editable `source:` clause (the facet's "Clear"). */
export function clearSources(ast: Ast | null | undefined): Ast {
	const first = editable(ast);
	return rebuild(conjuncts(ast).filter((n) => n !== first));
}

/** "Mainline DHT" → "DHT" for the short "Found by" line; other names are kept. */
export function shortName(name: string): string {
	return name.replace(/^Mainline\s+/, '');
}
