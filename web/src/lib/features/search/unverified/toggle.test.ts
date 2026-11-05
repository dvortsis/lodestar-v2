import { describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { showsUnverified, withUnverified } from './toggle';

const word: Ast = { op: 'word', field: 'any', word: 'dune' };
const both: Ast = { op: 'verified', any: ['yes', 'no'] };

describe('showsUnverified', () => {
	it('is false for no query and for a plain query', () => {
		expect(showsUnverified(null)).toBe(false);
		expect(showsUnverified({ op: 'all' })).toBe(false);
		expect(showsUnverified(word)).toBe(false);
	});
	it('is true when a top-level clause lists no', () => {
		expect(showsUnverified(both)).toBe(true);
		expect(showsUnverified({ op: 'verified', any: ['no'] })).toBe(true);
		expect(showsUnverified({ op: 'and', of: [word, both] })).toBe(true);
	});
	it('is false for verified:yes alone', () => {
		expect(showsUnverified({ op: 'verified', any: ['yes'] })).toBe(false);
	});
});

describe('withUnverified', () => {
	it('adds the clause to an empty query', () => {
		expect(withUnverified({ op: 'all' }, true)).toEqual(both);
		expect(withUnverified(null, true)).toEqual(both);
	});
	it('adds it beside other clauses', () => {
		expect(withUnverified(word, true)).toEqual({ op: 'and', of: [word, both] });
	});
	it('replaces a typed verified:no instead of doubling it', () => {
		const typed: Ast = { op: 'verified', any: ['no'] };
		expect(withUnverified({ op: 'and', of: [word, typed] }, true)).toEqual({
			op: 'and',
			of: [word, both]
		});
	});
	it('removes it again, leaving the rest', () => {
		expect(withUnverified({ op: 'and', of: [word, both] }, false)).toEqual(word);
		expect(withUnverified(both, false)).toEqual({ op: 'all' });
	});
	it('keeps a typed verified:yes when hiding', () => {
		const yes: Ast = { op: 'verified', any: ['yes'] };
		expect(withUnverified({ op: 'and', of: [yes, both] }, false)).toEqual(yes);
	});
	it('does not change the input', () => {
		const input: Ast = { op: 'and', of: [word, both] };
		const copy = structuredClone(input);
		withUnverified(input, false);
		expect(input).toEqual(copy);
	});
});
