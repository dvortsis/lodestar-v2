// "Show unverified" as an AST edit (SRC-3: unverified items are hidden unless the query asks for
// them). On adds the top-level clause `verified:yes,no`; off removes it. Pure: the caller hands the
// result to `search.setAst`.
import type { Ast } from '$lib/state/search.svelte';

type Verified = Extract<Ast, { op: 'verified' }>;

const conjuncts = (ast: Ast | null | undefined): Ast[] =>
	!ast || ast.op === 'all' ? [] : ast.op === 'and' ? ast.of : [ast];

const rebuild = (of: Ast[]): Ast => {
	const [only] = of;
	if (!only) return { op: 'all' };
	return of.length === 1 ? only : { op: 'and', of };
};

const isVerified = (node: Ast): node is Verified => node.op === 'verified';

/** True when a top-level clause lets unverified items in (`verified:no` or `verified:yes,no`). */
export function showsUnverified(ast: Ast | null | undefined): boolean {
	return conjuncts(ast).some((node) => isVerified(node) && node.any.includes('no'));
}

/** The query with unverified items shown (`on`) or hidden again; other clauses are kept. */
export function withUnverified(ast: Ast | null | undefined, on: boolean): Ast {
	const rest: Ast[] = conjuncts(ast).filter((node) => !isVerified(node));
	if (on) {
		const shown: Ast = { op: 'verified', any: ['yes', 'no'] };
		rest.push(shown);
	} else {
		// Hiding again keeps a `verified:yes` the user typed.
		const yes = conjuncts(ast).filter((n) => isVerified(n) && !n.any.includes('no'));
		rest.push(...yes);
	}
	return rebuild(rest);
}
