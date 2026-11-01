// The Builder's field list: from the server, per operator type, refetched when Adult's visibility
// changes, with a failure state, and no hard-coded `adult` anywhere in the assist code.
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { readFileSync, readdirSync } from 'node:fs';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import Builder from './Builder.svelte';
import { createFields, opsOf } from './fields.svelte';

const LAZY = { timeout: 10_000 };
const json = (body: unknown, status = 200) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
const base = [
	{ name: 'res', aliases: [], type: 'keyword', operators: ['eq', 'any_of'], facetable: true },
	{
		name: 'year',
		aliases: [],
		type: 'number',
		operators: ['eq', 'gt', 'gte', 'lt', 'lte', 'range'],
		facetable: false
	},
	{ name: 'discovered', aliases: ['age'], type: 'date', operators: ['lt', 'gt'], facetable: false },
	{
		name: 'type',
		aliases: [],
		type: 'enum',
		operators: ['eq'],
		values: ['movie', 'tv'],
		facetable: true
	}
];
const adultFields = [
	...base,
	{
		name: 'adult',
		aliases: [],
		type: 'enum',
		operators: ['eq'],
		values: ['scene'],
		facetable: true
	}
];
let adultOn = false;
let failing = false;

beforeEach(() => {
	adultOn = false;
	failing = false;
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => {
			if (failing) return json({ error: { code: 'internal', message: 'x' } }, 500);
			return json({ fields: adultOn ? adultFields : base });
		})
	);
});
afterEach(() => vi.unstubAllGlobals());

const optionsOf = (select: HTMLElement) =>
	within(select)
		.getAllByRole('option')
		.map((o) => o.textContent);

const props = () => ({
	ast: { op: 'all' } as never,
	apply: vi.fn(async () => {}),
	close: () => {},
	announce: () => {}
});

describe('operators per type', () => {
	it('maps the server operators and defaults to is', () => {
		const f = (type: string, operators: string[]) => ({ type, operators }) as never;
		expect(opsOf(f('number', base[1]!.operators))).toEqual([':', '>', '>=', '<', '<=']);
		expect(opsOf(f('date', ['lt', 'gt']))).toEqual(['<', '>']);
		expect(opsOf(f('keyword', ['eq', 'any_of']))).toEqual([':']);
		expect(opsOf(undefined)).toEqual([':']);
		expect(opsOf(f('share', ['gte', 'lte']))).toEqual([':']);
	});
});

describe('the field store', () => {
	it('finds a field, drops the list while asking again, and retries after a failure', async () => {
		const s = createFields();
		s.ensure();
		await waitFor(() => expect(s.status).toBe('ready'), LAZY);
		expect(s.find('discovered')?.aliases).toEqual(['age']);
		failing = true;
		const again = s.ensure();
		expect(s.fields).toEqual([]);
		await again;
		expect(s.status).toBe('error');
		failing = false;
		await s.refresh();
		expect(s.status).toBe('ready');
		expect(s.fields).toHaveLength(base.length);
	});
});

describe('the Builder', () => {
	it('lists the server fields and offers a Select for a fixed list of values', async () => {
		render(Builder, props());
		const panel = await screen.findByRole('region', { name: 'Query builder' });
		const draft = await within(panel).findByRole('group', { name: 'New filter' }, LAZY);
		const field = within(draft).getByLabelText('Field');
		expect(optionsOf(field)).toEqual(['res', 'year', 'discovered', 'type']);
		const op = () => optionsOf(within(draft).getByLabelText('Operator'));
		await fireEvent.change(field, { target: { value: 'discovered' } });
		expect(op()).toEqual(['less than', 'more than']);
		await fireEvent.change(field, { target: { value: 'year' } });
		expect(op()).toHaveLength(5);
		await fireEvent.change(field, { target: { value: 'type' } });
		const value = within(draft).getByLabelText('Value');
		expect(value.tagName).toBe('SELECT');
		expect([...value.querySelectorAll('option')].map((o) => o.value)).toEqual(['', 'movie', 'tv']);
	});

	it('asks again when it opens: Adult shown once the setting is on, gone when it is off', async () => {
		const open = async () => {
			const view = render(Builder, props());
			const draft = await view.findByRole('group', { name: 'New filter' }, LAZY);
			const names = optionsOf(within(draft).getByLabelText('Field'));
			view.unmount();
			return names;
		};
		expect(await open()).not.toContain('adult');
		adultOn = true;
		expect(await open()).toContain('adult');
		adultOn = false;
		expect(await open()).not.toContain('adult');
	});

	it('keeps a row with several listed values as text, and a share field as text', async () => {
		const view = render(Builder, {
			...props(),
			ast: { op: 'tag', key: 'type', any: ['movie', 'tv'] } as never
		});
		const row = await view.findByRole('group', { name: /movie/i }, LAZY);
		const value = within(row).getByLabelText('Value') as HTMLInputElement;
		expect(value.tagName).toBe('INPUT');
		expect(value.value).toBe('movie,tv');
	});

	it('says so when the fields fail, offers Retry, and never lists a built-in set', async () => {
		failing = true;
		render(Builder, props());
		const panel = await screen.findByRole('region', { name: 'Query builder' });
		expect(await within(panel).findByText(/Couldn't load the fields/, {}, LAZY)).toBeTruthy();
		expect(within(panel).queryByLabelText('Field')).toBeNull();
		failing = false;
		await fireEvent.click(within(panel).getByRole('button', { name: 'Retry' }));
		await waitFor(() => expect(within(panel).getByLabelText('Field')).toBeTruthy(), LAZY);
	});
});

describe('no hard-coded Adult', () => {
	it('keeps `adult` out of the assist code', () => {
		for (const f of readdirSync(__dirname)) {
			if (/\.test\.ts$/.test(f) || !/\.(ts|svelte|css)$/.test(f)) continue;
			const text = readFileSync(`${__dirname}/${f}`, 'utf8');
			expect(text.toLowerCase(), f).not.toMatch(/\badult\b/);
		}
	});
});
