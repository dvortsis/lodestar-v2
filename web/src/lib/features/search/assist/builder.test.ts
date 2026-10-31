import { describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { FIELDS, OPERATORS, kindOf, rowOf, rowText, withNode } from './builder';

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const res = (v: string): Ast => ({ op: 'tag', key: 'res', any: [v] });

describe('rowText', () => {
	it('joins field, operator and value for the server to parse', () => {
		expect(rowText({ field: 'res', op: ':', value: ' 2160p ' })).toBe('res:2160p');
		expect(rowText({ field: 'year', op: '>=', value: '2020' })).toBe('year:>=2020');
		expect(rowText({ field: 'discovered', op: '<', value: '7d' })).toBe('discovered:<7d');
	});
});

describe('rowOf', () => {
	it('reads tags, extensions, scoped words and one-sided ranges', () => {
		expect(rowOf(res('2160p'))).toEqual({ field: 'res', op: ':', value: '2160p' });
		expect(rowOf({ op: 'ext', any: ['mkv', 'mp4'] })).toEqual({
			field: 'ext',
			op: ':',
			value: 'mkv,mp4'
		});
		expect(rowOf({ op: 'word', field: 'title', word: 'dune' })).toEqual({
			field: 'title',
			op: ':',
			value: 'dune'
		});
		expect(rowOf({ op: 'range', field: 'year', lo: 2020, hi: null })).toEqual({
			field: 'year',
			op: '>=',
			value: '2020'
		});
		expect(rowOf({ op: 'range', field: 'files', lo: null, hi: 9 })!.op).toBe('<=');
		expect(rowOf({ op: 'range', field: 'year', lo: 2020, hi: 2020 })!.op).toBe(':');
	});
	it('leaves everything else read-only', () => {
		expect(rowOf(word('free'))).toBeNull();
		expect(rowOf({ op: 'range', field: 'size', lo: 1, hi: null })).toBeNull();
		expect(rowOf({ op: 'range', field: 'year', lo: 2019, hi: 2021 })).toBeNull();
		expect(rowOf({ op: 'not', of: res('720p') })).toBeNull();
	});
});

describe('withNode', () => {
	it('replaces a clause, appends a new one, and collapses a single clause', () => {
		const root: Ast = { op: 'and', of: [word('a'), res('720p')] };
		expect(withNode(root, 1, res('1080p'))).toEqual({ op: 'and', of: [word('a'), res('1080p')] });
		expect(withNode(root, null, res('4k'))).toEqual({
			op: 'and',
			of: [word('a'), res('720p'), res('4k')]
		});
		expect(withNode(null, null, res('4k'))).toEqual(res('4k'));
		expect(withNode({ op: 'all' }, null, res('4k'))).toEqual(res('4k'));
	});
});

describe('fields', () => {
	it('has an operator list for every kind and no duplicate names', () => {
		expect(new Set(FIELDS.map((f) => f.name)).size).toBe(FIELDS.length);
		for (const f of FIELDS) expect(OPERATORS[kindOf(f.name)].length).toBeGreaterThan(0);
	});
});

describe('rows and text fields', () => {
	it('leaves byte-valued ranges and the other tag keys right', () => {
		for (const field of ['largest', 'piece'] as const)
			expect(rowOf({ op: 'range', field, lo: 1, hi: null })).toBeNull();
		expect(FIELDS.map((f) => f.name)).toContain('resolved');
		expect(rowOf({ op: 'tag', key: 'adult', any: ['no'] })!.field).toBe('adult');
		expect(FIELDS.map((f) => f.name)).toContain('adult');
	});
	it('quotes words in a text field', () => {
		expect(rowText({ field: 'title', op: ':', value: 'dark knight' })).toBe('title:"dark knight"');
		expect(rowText({ field: 'title', op: ':', value: 'dune' })).toBe('title:dune');
	});
});
