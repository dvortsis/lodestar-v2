// Facet clicks as AST edits (§10.4: one state, the AST). Pure: the rail hands the result to W5
// (`ensureSearchPath()`, then `search.setAst`), which prints it back as text, one history entry.
//
// A facet's own clauses are what the server counts disjunctively (crates/search facets.rs): the
// top-level conjuncts built only from the facet's field, a leaf (`res:1080p`), an OR/AND of them,
// or a negation of one. A toggle rewrites those into at most two clauses, in place of the first:
// the selected values (`res:1080p,2160p`) and the excluded ones (`-res:480p`). Removing the last
// value drops the clause. Ranges and dates replace the field's top-level range clause.
import type { components } from '$lib/api/schema';
import type { Ast } from '$lib/state/search.svelte';

type TagKey = components['schemas']['AstTagKey'];
type NumField = components['schemas']['AstNumField'];
type DateField = components['schemas']['AstDateField'];
export type When = components['schemas']['AstWhen'];

export type FacetField =
	{ kind: 'tag'; key: TagKey } | { kind: 'ext' } | { kind: 'flag'; name: string };

const TAGS: readonly string[] = [
	'type',
	'res',
	'hdr',
	'codec',
	'audio',
	'lang',
	'group',
	'has',
	'adult'
] satisfies TagKey[];
const FLAGS: readonly string[] = ['private', 'v2', 'ambiguous', 'imported', 'backfill'];

/** The query field a server facet counts (`/facets` names), or null for one the rail can't set. */
export function fieldOf(facet: string): FacetField | null {
	if (TAGS.includes(facet)) return { kind: 'tag', key: facet as TagKey };
	if (facet === 'ext') return { kind: 'ext' };
	if (FLAGS.includes(facet)) return { kind: 'flag', name: facet };
	return null;
}

function conjuncts(ast: Ast | null | undefined): Ast[] {
	if (!ast || ast.op === 'all') return [];
	return ast.op === 'and' ? ast.of : [ast];
}

function rebuild(of: Ast[]): Ast {
	const [only] = of;
	if (!only) return { op: 'all' };
	return of.length === 1 ? only : { op: 'and', of };
}

/** A leaf's values on `field`, or null when the node isn't a leaf on it. */
function leafValues(node: Ast, field: FacetField): string[] | null {
	if (field.kind === 'tag' && node.op === 'tag' && node.key === field.key) return node.any;
	if (field.kind === 'ext' && node.op === 'ext') return node.any;
	if (field.kind === 'flag' && node.op === 'flag' && node.name === field.name)
		return [node.value ? 'yes' : 'no'];
	return null;
}

/** The server's `Facet::owns`: a leaf on the field, or AND/OR/NOT made only of such leaves. */
export function owns(node: Ast, field: FacetField): boolean {
	if (node.op === 'and' || node.op === 'or')
		return node.of.length > 0 && node.of.every((n) => owns(n, field));
	if (node.op === 'not') return owns(node.of, field);
	return leafValues(node, field) !== null;
}

function collect(node: Ast, field: FacetField, negated: boolean, out: Selection) {
	if (node.op === 'and' || node.op === 'or') {
		for (const n of node.of) collect(n, field, negated, out);
		return;
	}
	if (node.op === 'not') return collect(node.of, field, !negated, out);
	const into = negated ? out.exclude : out.include;
	for (const v of leafValues(node, field) ?? []) if (!into.includes(v)) into.push(v);
}

export interface Selection {
	include: string[];
	exclude: string[];
}

/** The values the query's own clauses on `field` select and exclude. */
export function selection(ast: Ast | null | undefined, field: FacetField): Selection {
	const out: Selection = { include: [], exclude: [] };
	for (const c of conjuncts(ast)) if (owns(c, field)) collect(c, field, false, out);
	return out;
}

function leaf(field: FacetField, values: string[]): Ast {
	if (field.kind === 'tag') return { op: 'tag', key: field.key, any: values };
	if (field.kind === 'ext') return { op: 'ext', any: values };
	const flags: Ast[] = values.map((v) => ({ op: 'flag', name: field.name, value: v === 'yes' }));
	return flags.length === 1 ? flags[0]! : { op: 'or', of: flags };
}

/** Replaces the conjuncts `match` picks with `next`, at the first one's place (else the end). */
function replace(ast: Ast | null | undefined, match: (n: Ast) => boolean, next: Ast[]): Ast {
	const all = conjuncts(ast);
	const at = all.findIndex(match);
	const kept = all.filter((n) => !match(n));
	const index = at < 0 ? kept.length : all.slice(0, at).filter((n) => !match(n)).length;
	kept.splice(index, 0, ...next);
	return rebuild(kept);
}

/**
 * An own clause a toggle can rewrite without changing what it means: a leaf, an OR of leaves, or
 * the negation of either. Other own clauses (`-(res:a & res:b)`, nested mixes) are left as they
 * are: rewriting them into "selected" and "excluded" lists would change the query's logic.
 */
function rewritable(node: Ast, field: FacetField): boolean {
	const plain = (n: Ast) =>
		leafValues(n, field) !== null ||
		(n.op === 'or' && n.of.length > 0 && n.of.every((c) => leafValues(c, field) !== null));
	return plain(node) || (node.op === 'not' && plain(node.of));
}

/**
 * Selects `value` (or excludes it, `how: 'exclude'`), or undoes that when it already is.
 * Selecting an excluded value moves it across, and the reverse. A flag has one value at a time:
 * `yes` or `no` (excluding one selects the other).
 */
export function toggleValue(
	ast: Ast | null | undefined,
	field: FacetField,
	value: string,
	how: 'include' | 'exclude' = 'include'
): Ast {
	const mine = (n: Ast) => owns(n, field) && rewritable(n, field);
	const sel: Selection = { include: [], exclude: [] };
	for (const c of conjuncts(ast)) if (mine(c)) collect(c, field, false, sel);
	if (field.kind === 'flag') {
		const want = how === 'include' ? value : value === 'yes' ? 'no' : 'yes';
		const on = sel.include.length === 1 && sel.include[0] === want && sel.exclude.length === 0;
		return replace(ast, mine, on ? [] : [leaf(field, [want])]);
	}
	const [these, other] =
		how === 'include' ? [sel.include, sel.exclude] : [sel.exclude, sel.include];
	const had = these.includes(value);
	const nextMine = had ? these.filter((v) => v !== value) : [...these, value];
	const nextOther = other.filter((v) => v !== value);
	const [include, exclude] = how === 'include' ? [nextMine, nextOther] : [nextOther, nextMine];
	const next: Ast[] = [];
	if (include.length) next.push(leaf(field, include));
	if (exclude.length) next.push({ op: 'not', of: leaf(field, exclude) });
	return replace(ast, mine, next);
}

/** Drops every value of the facet (its "Clear"). */
export function clearField(ast: Ast | null | undefined, field: FacetField): Ast {
	return replace(ast, (n) => owns(n, field), []);
}

const isRange = (field: NumField) => (n: Ast) => n.op === 'range' && n.field === field;
const isDate = (field: DateField) => (n: Ast) => n.op === 'date' && n.field === field;

/** The field's top-level range clause (inclusive bounds), or null. */
export function rangeOf(
	ast: Ast | null | undefined,
	field: NumField
): { lo: number | null; hi: number | null } | null {
	const n = conjuncts(ast).find(isRange(field));
	return n && n.op === 'range' ? { lo: n.lo, hi: n.hi } : null;
}

/** Replaces the field's range clause (inclusive bounds); both null removes it. */
export function setRange(
	ast: Ast | null | undefined,
	field: NumField,
	lo: number | null,
	hi: number | null
): Ast {
	const next: Ast[] = lo === null && hi === null ? [] : [{ op: 'range', field, lo, hi }];
	return replace(ast, isRange(field), next);
}

/** The field's top-level date clause, or null. */
export function dateOf(
	ast: Ast | null | undefined,
	field: DateField
): { from: When | null; to: When | null } | null {
	const n = conjuncts(ast).find(isDate(field));
	return n && n.op === 'date' ? { from: n.from, to: n.to } : null;
}

/** Replaces the field's date clause (inclusive ends); both null removes it. */
export function setDate(
	ast: Ast | null | undefined,
	field: DateField,
	from: When | null,
	to: When | null
): Ast {
	const next: Ast[] = from === null && to === null ? [] : [{ op: 'date', field, from, to }];
	return replace(ast, isDate(field), next);
}
