import { describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { clearSources, fixedSources, selectedSources, shortName, toggleSource } from './source';

const word: Ast = { op: 'word', field: 'any', word: 'dune' };
describe('source facet edits', () => {
	it('adds a source beside other clauses and removes it again', () => {
		const on = toggleSource(word, 'nyaa');
		expect(on).toEqual({ op: 'and', of: [word, { op: 'source', any: ['nyaa'] }] });
		expect(toggleSource(on, 'nyaa')).toEqual(word);
	});
	it('merges into the existing clause in place', () => {
		const ast: Ast = { op: 'and', of: [{ op: 'source', any: ['dht'] }, word] };
		expect(toggleSource(ast, 'nyaa')).toEqual({
			op: 'and',
			of: [{ op: 'source', any: ['dht', 'nyaa'] }, word]
		});
		expect(selectedSources(toggleSource(ast, 'nyaa'))).toEqual(['dht', 'nyaa']);
	});
	it('goes back to match-all when the last one goes', () => {
		expect(toggleSource({ op: 'source', any: ['dht'] }, 'dht')).toEqual({ op: 'all' });
		expect(clearSources({ op: 'source', any: ['dht'] })).toEqual({ op: 'all' });
	});
	it('shortens Mainline DHT only', () => {
		expect(shortName('Mainline DHT')).toBe('DHT');
		expect(shortName('Nyaa')).toBe('Nyaa');
	});
});

describe('source facet: clauses a click must not rewrite', () => {
	const dht: Ast = { op: 'source', any: ['dht'] };
	const nyaa: Ast = { op: 'source', any: ['nyaa'] };
	it('leaves a second positive clause alone (it is an AND) and disables its values', () => {
		const ast: Ast = { op: 'and', of: [dht, nyaa] };
		expect(selectedSources(ast)).toEqual(['dht']);
		expect(fixedSources(ast)).toEqual(new Set(['nyaa']));
		expect(toggleSource(ast, 'x')).toEqual({
			op: 'and',
			of: [{ op: 'source', any: ['dht', 'x'] }, nyaa]
		});
	});
	it('leaves a negated clause alone and disables its values', () => {
		const ast: Ast = { op: 'not', of: dht };
		expect(selectedSources(ast)).toEqual([]);
		expect(fixedSources(ast)).toEqual(new Set(['dht']));
		expect(toggleSource(ast, 'nyaa')).toEqual({ op: 'and', of: [ast, nyaa] });
	});
	it('clears only the editable clause', () => {
		const neg: Ast = { op: 'not', of: dht };
		expect(clearSources({ op: 'and', of: [nyaa, neg] })).toEqual(neg);
	});
});
