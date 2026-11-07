import { describe, expect, it } from 'vitest';
import {
	blankRow,
	initialValues,
	parseSchema,
	toConfig,
	validate,
	type ListField,
	type ScalarField,
	type Values
} from './config-form';

const schema = (properties: Record<string, unknown>, required: string[] = []) => ({
	type: 'object',
	properties,
	required
});
const prowlarr = schema(
	{
		url: { type: 'string', format: 'uri', title: 'Address', description: 'Where it is' },
		manager: { type: 'string', enum: ['prowlarr', 'jackett'], default: 'prowlarr' },
		agents: { type: 'integer', minimum: 1, maximum: 8, default: 2, title: 'Indexers' },
		verbose: { type: 'boolean', default: true }
	},
	['url', 'manager']
);
const caches = schema(
	{
		caches: {
			type: 'array',
			items: { type: 'string', maxLength: 10 },
			maxItems: 2,
			uniqueItems: true
		}
	},
	['caches']
);

describe('parseSchema', () => {
	it('reads string, enum, integer and boolean fields with their bounds', () => {
		const f = parseSchema(prowlarr)!;
		expect(f.map((x) => [x.name, x.kind, x.required])).toEqual([
			['url', 'string', true],
			['manager', 'enum', true],
			['agents', 'integer', false],
			['verbose', 'boolean', false]
		]);
		const agents = f[2] as ScalarField;
		expect([agents.min, agents.max, agents.label]).toEqual([1, 8, 'Indexers']);
		expect((f[1] as ScalarField).options).toEqual(['prowlarr', 'jackett']);
		expect(f[0]!.help).toBe('Where it is');
	});

	it('reads arrays of strings and of flat objects', () => {
		const f = parseSchema(
			schema({
				a: { type: 'array', items: { type: 'string' }, minItems: 1 },
				b: {
					type: 'array',
					items: {
						type: 'object',
						properties: { host: { type: 'string' }, port: { type: 'integer' } },
						required: ['host']
					}
				}
			})
		)!;
		expect(f.map((x) => x.kind)).toEqual(['strings', 'objects']);
		expect((f[1] as ListField).item.map((i) => [i.name, i.required])).toEqual([
			['host', true],
			['port', false]
		]);
	});

	it('marks what it cannot draw as unsupported instead of guessing', () => {
		const f = parseSchema(
			schema({
				nested: { type: 'object', properties: { x: { type: 'string' } } },
				either: { oneOf: [{ type: 'string' }, { type: 'integer' }] },
				ref: { $ref: '#/$defs/x' },
				mixed: { type: ['string', 'null'] },
				numbers: { type: 'array', items: { type: 'integer' } },
				deep: { type: 'array', items: { type: 'object', properties: { a: { type: 'array' } } } },
				numEnum: { type: 'integer', enum: [1, 2] },
				ok: { type: 'string' }
			})
		)!;
		expect(f.map((x) => x.kind)).toEqual([
			'unsupported',
			'unsupported',
			'unsupported',
			'unsupported',
			'unsupported',
			'unsupported',
			'unsupported',
			'string'
		]);
	});

	it('returns null when the schema is not an object with properties', () => {
		for (const bad of [null, 3, { type: 'string' }, { type: 'object' }, { type: 'array' }])
			expect(parseSchema(bad)).toBeNull();
	});
});

describe('values and checks', () => {
	it('starts from defaults', () => {
		const v = initialValues(parseSchema(prowlarr)!);
		expect(v).toEqual({ url: '', manager: 'prowlarr', agents: '2', verbose: true });
	});

	it('checks required, type and bounds like the server', () => {
		const f = parseSchema(prowlarr)!;
		const v = initialValues(f);
		expect(Object.keys(validate(f, v))).toEqual(['url']);
		expect(validate(f, { ...v, url: 'http://x', agents: '9' }).agents).toContain('8 or less');
		expect(validate(f, { ...v, url: 'http://x', agents: '0' }).agents).toContain('1 or more');
		expect(validate(f, { ...v, url: 'http://x', agents: '1.5' }).agents).toContain('whole');
		expect(validate(f, { ...v, url: 'http://x', agents: 'abc' }).agents).toContain('whole');
		expect(validate(f, { ...v, url: 'http://x', agents: '0x10' }).agents).toContain('whole');
		expect(validate(f, { ...v, url: 'http://x', manager: 'other' }).manager).toBeTruthy();
		expect(validate(f, { ...v, url: 'http://x' })).toEqual({});
		expect(validate(f, { ...v, url: 'http://x', agents: '' })).toEqual({});
	});

	it('checks list size, element length and duplicates', () => {
		const f = parseSchema(caches)!;
		// `required` means the key is sent, not that it is non-empty: [] is the server's to judge.
		expect(validate(f, { caches: [] })).toEqual({});
		expect(validate(f, { caches: ['a', 'b', 'c'] }).caches).toContain('2 at most');
		expect(validate(f, { caches: ['abcdefghijkl'] })['caches.0']).toContain('10 characters');
		expect(validate(f, { caches: ['a', 'a'] })['caches.1']).toContain('repeats');
		expect(validate(f, { caches: ['a', ' ', 'b'] })).toEqual({});
	});

	it('checks the cells of object rows', () => {
		const f = parseSchema(
			schema({
				rows: {
					type: 'array',
					items: {
						type: 'object',
						properties: { host: { type: 'string' }, port: { type: 'integer', maximum: 10 } },
						required: ['host']
					}
				}
			})
		)!;
		const list = f[0] as ListField;
		const row = { ...blankRow(list), port: '11' };
		expect(validate(f, { rows: [row] } as Values)).toEqual({
			'rows.0.host': 'Enter a value.',
			'rows.0.port': 'Enter 10 or less.'
		});
		// A row left entirely blank is dropped, not an error.
		expect(validate(f, { rows: [blankRow(list)] } as Values)).toEqual({});
	});
});

describe('toConfig', () => {
	it('types the values and leaves blank optional ones out', () => {
		const f = parseSchema(prowlarr)!;
		const v = { ...initialValues(f), url: ' http://x ', agents: '' };
		expect(toConfig(f, v)).toEqual({ url: 'http://x', manager: 'prowlarr', verbose: true });
		expect(toConfig(f, { ...v, agents: '3', verbose: false })).toMatchObject({
			agents: 3,
			verbose: false
		});
	});

	it('trims lists, drops blank rows and sends nothing for unsupported settings', () => {
		const f = parseSchema(
			schema({ caches: { type: 'array', items: { type: 'string' } }, odd: { oneOf: [] } })
		)!;
		expect(toConfig(f, { caches: [' a ', '', 'b'] })).toEqual({ caches: ['a', 'b'] });
	});
});

describe('lists with defaults and boolean cells', () => {
	const f = parseSchema(
		schema({
			rows: {
				type: 'array',
				default: [{ host: 'a.example', tls: true }],
				items: {
					type: 'object',
					properties: { host: { type: 'string' }, tls: { type: 'boolean' } },
					required: ['host']
				}
			},
			names: { type: 'array', default: ['x'], items: { type: 'string' } }
		})
	)!;

	it('starts lists from their defaults, so an untouched list is not emptied', () => {
		const v = initialValues(f);
		expect(v.names).toEqual(['x']);
		expect(toConfig(f, v)).toEqual({ rows: [{ host: 'a.example', tls: true }], names: ['x'] });
	});

	it('treats a row of untouched defaults as blank on both sides', () => {
		const list = f[0] as ListField;
		const v = { ...initialValues(f), rows: [blankRow(list)] } as Values;
		expect(validate(f, v)).toEqual({});
		expect(toConfig(f, v).rows).toEqual([]);
	});
});
