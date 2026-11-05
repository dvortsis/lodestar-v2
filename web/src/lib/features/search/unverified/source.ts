// The Source facet as AST edits: a click adds or removes a plugin id in the query's top-level
// `source:` clause. Include only (the facet offers no exclusion).
import type { Ast } from '$lib/state/search.svelte';

type SourceNode = Extract<Ast, { op: 'source' }>;
const isSource = (n: Ast): n is SourceNode => n.op === 'source';
const conjuncts = (ast: Ast | null | undefined): Ast[] =>
	!ast || ast.op === 'all' ? [] : ast.op === 'and' ? ast.of : [ast];

/** The plugin ids the query's top-level `source:` clauses select. */
export function selectedSources(ast: Ast | null | undefined): string[] {
	return [...new Set(conjuncts(ast).flatMap((n) => (isSource(n) ? n.any : [])))];
}

/** Adds `id` to the query's `source:` clause, or removes it when it is already there. */
export function toggleSource(ast: Ast | null | undefined, id: string): Ast {
	const all = conjuncts(ast);
	const current = selectedSources(ast);
	const next = current.includes(id) ? current.filter((v) => v !== id) : [...current, id];
	const at = all.findIndex(isSource);
	const rest: Ast[] = all.filter((n) => !isSource(n));
	if (next.length) {
		const clause: Ast = { op: 'source', any: next };
		rest.splice(at < 0 ? rest.length : at, 0, clause);
	}
	const [only] = rest;
	return !only ? { op: 'all' } : rest.length === 1 ? only : { op: 'and', of: rest };
}

/** Removes every `source:` clause. */
export function clearSources(ast: Ast | null | undefined): Ast {
	const rest: Ast[] = conjuncts(ast).filter((n) => !isSource(n));
	const [only] = rest;
	return !only ? { op: 'all' } : rest.length === 1 ? only : { op: 'and', of: rest };
}

/** "Mainline DHT" → "DHT" for the short "Found by" line; other names are kept. */
export function shortName(name: string): string {
	return name.replace(/^Mainline\s+/, '');
}
