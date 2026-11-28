import { describe, expect, it } from 'vitest';
import type { Field } from './fields.svelte';
import { correct } from './rescue';

const f = (name: string, operators: Field['operators'], aliases: string[] = []) =>
	({ name, operators, aliases, type: 'number', facetable: false }) as unknown as Field;
const fields = [
	f('seeders', ['eq', 'gt', 'gte', 'lt', 'lte']),
	f('size', ['gt', 'gte', 'lt', 'lte']),
	f('discovered', ['gt', 'lt'], ['age']),
	f('res', ['eq'])
];

describe('correct (a filter typed as words)', () => {
	it('writes the clause the user meant', () => {
		expect(correct('seeders>5', fields)).toEqual({ text: 'seeders:>5', clauses: ['seeders:>5'] });
	});
	it('reads spaces around the comparison', () => {
		expect(correct('big size > 4gb', fields)?.text).toBe('big size:>4gb');
	});
	it('keeps the other words and fixes each fragment', () => {
		expect(correct('x265 seeders>=5 size<2gb', fields)).toEqual({
			text: 'x265 seeders:>=5 size:<2gb',
			clauses: ['seeders:>=5', 'size:<2gb']
		});
	});
	it('maps = to the colon form and knows aliases', () => {
		expect(correct('seeders=5', fields)?.text).toBe('seeders:5');
		expect(correct('age>7d', fields)?.text).toBe('age:>7d');
	});
	it('leaves finished clauses, unknown words and unsupported operators alone', () => {
		expect(correct('seeders:>5', fields)).toBeNull();
		expect(correct('score>5', fields)).toBeNull();
		expect(correct('res>1080p', fields)).toBeNull();
		expect(correct('lantern keeper', fields)).toBeNull();
	});
	it('leaves a quoted phrase alone', () => {
		expect(correct('"size>4gb is big"', fields)).toBeNull();
		expect(correct('"a b" size>4gb', fields)?.text).toBe('"a b" size:>4gb');
	});
	it('holds no field names of its own: with no list there is no rescue', () => {
		expect(correct('seeders>5', [])).toBeNull();
	});
});
