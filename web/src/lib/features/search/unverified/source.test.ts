import { describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { clearField, fieldOf, fixedValues, selection, toggleValue } from '../facets/toggle';
import { sourceLabel } from '$lib/format/labels';

// The Source facet is `facets/toggle.ts`'s `source` kind: include only here, same rules as the rest.
const field = fieldOf('source')!;
const word: Ast = { op: 'word', field: 'any', word: 'dune' };
const dht: Ast = { op: 'source', any: ['dht'] };
const nyaa: Ast = { op: 'source', any: ['nyaa'] };

describe('source facet edits', () => {
	it('is a known facet field', () => {
		expect(field).toEqual({ kind: 'source' });
	});
	it('adds a source beside other clauses and removes it again', () => {
		const on = toggleValue(word, field, 'nyaa');
		expect(on).toEqual({ op: 'and', of: [word, nyaa] });
		expect(toggleValue(on, field, 'nyaa')).toEqual(word);
	});
	it('merges into the existing clause in place', () => {
		const ast: Ast = { op: 'and', of: [dht, word] };
		const next = toggleValue(ast, field, 'nyaa');
		expect(next).toEqual({ op: 'and', of: [{ op: 'source', any: ['dht', 'nyaa'] }, word] });
		expect(selection(next, field).include).toEqual(['dht', 'nyaa']);
	});
	it('goes back to match-all when the last one goes', () => {
		expect(toggleValue(dht, field, 'dht')).toEqual({ op: 'all' });
		expect(clearField(dht, field)).toEqual({ op: 'all' });
	});
	it('leaves a second positive clause alone (it is an AND) and disables its values', () => {
		const ast: Ast = { op: 'and', of: [dht, nyaa] };
		expect(fixedValues(ast, field)).toEqual(new Set(['nyaa']));
		expect(toggleValue(ast, field, 'x')).toEqual({
			op: 'and',
			of: [{ op: 'source', any: ['dht', 'x'] }, nyaa]
		});
	});
	it('keeps a negated clause as an exclusion', () => {
		const neg: Ast = { op: 'not', of: dht };
		expect(selection(neg, field).exclude).toEqual(['dht']);
		expect(toggleValue({ op: 'and', of: [word, neg] }, field, 'nyaa')).toEqual({
			op: 'and',
			of: [word, nyaa, neg]
		});
	});
});

describe('the Source facet names (X-labels: sourceLabel replaces shortName)', () => {
	it('shows the DHT as "DHT" and other sources by their names, else the id', () => {
		expect(sourceLabel('dht', 'Mainline DHT')).toBe('DHT');
		expect(sourceLabel('nyaa', 'Nyaa')).toBe('Nyaa');
		expect(sourceLabel('gone', 'gone')).toBe('gone');
	});
});
