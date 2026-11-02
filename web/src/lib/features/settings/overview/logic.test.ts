import { describe, expect, it } from 'vitest';
import type { components } from '$lib/api/schema';
import type { FieldKey, FieldSpec, SectionValues } from '../form/types';
import { changedFromDefault, needsAttention } from './logic';

const spec = (section: string, name: string, over: Partial<FieldSpec> = {}): FieldSpec =>
	({ key: `${section}.${name}`, section, name, writeOnly: false, ...over }) as FieldSpec;
const v = (value: unknown, source = 'default', env: string | null = null) => ({
	value,
	source,
	env,
	read_only: false,
	restart_required: false
});
const section = (name: string, fields: Record<string, unknown>): SectionValues =>
	({ section: name, revision: 1, restart_required: [], fields }) as SectionValues;

describe('changedFromDefault', () => {
	const specs = new Map(
		[
			spec('crawler', 'intensity'),
			spec('crawler', 'paused'),
			spec('integrations', 'clients', { kind: 'map' }),
			spec('integrations', 'api_key', { writeOnly: true }),
			spec('mystery', 'x')
		].map((s) => [s.key, s])
	);
	const specOf = (key: FieldKey) => specs.get(key);
	const screenOf = (s: string) =>
		s === 'crawler' ? 'crawler' : s === 'integrations' ? 'integrations' : undefined;

	it('lists only values that are not at their default, grouped by screen', () => {
		const groups = changedFromDefault(
			specOf,
			[
				section('crawler', { intensity: v('low', 'db'), paused: v(false) }),
				section('mystery', { x: v(1, 'file') })
			],
			screenOf
		);
		expect(groups.map((g) => [g.id, g.linked, g.rows.map((r) => r.spec.name)])).toEqual([
			['crawler', true, ['intensity']],
			['mystery', false, ['x']]
		]);
	});

	it('never carries a secret value', () => {
		const [g] = changedFromDefault(
			specOf,
			[section('integrations', { api_key: v('hunter2', 'db') })],
			screenOf
		);
		expect(g!.rows[0]).toMatchObject({ secret: true, source: 'db' });
		expect('value' in g!.rows[0]!).toBe(false);
		expect(JSON.stringify(g)).not.toContain('hunter2');
	});

	it('keeps nested values whole', () => {
		const nested = { a: { b: [1, 2] } };
		const [g] = changedFromDefault(
			specOf,
			[section('integrations', { clients: v(nested, 'env', 'LODESTAR_X') })],
			screenOf
		);
		expect(g!.rows[0]).toMatchObject({ value: nested, env: 'LODESTAR_X', source: 'env' });
	});

	it('is empty when everything is at its default', () => {
		expect(
			changedFromDefault(specOf, [section('crawler', { paused: v(false) })], screenOf)
		).toEqual([]);
	});

	it('skips fields the schema does not describe', () => {
		expect(
			changedFromDefault(specOf, [section('crawler', { ghost: v(1, 'db') })], screenOf)
		).toEqual([]);
	});
});

type System = components['schemas']['SystemInfo'];
const system = (over: Record<string, unknown> = {}): System =>
	({
		address_change: null,
		data: { filesystem_warning: null },
		data_formats: { rebuild: null },
		diagnostics: { recent_errors: 0 },
		restart_required: [],
		...over
	}) as unknown as System;

describe('needsAttention', () => {
	it('is empty for a healthy install', () => {
		expect(needsAttention(system(), { reachable: true } as never)).toEqual([]);
	});

	it('treats unknown reachability and unknown error counts as no problem', () => {
		const s = system({ diagnostics: { recent_errors: null } });
		expect(needsAttention(s, { reachable: null } as never)).toEqual([]);
		expect(needsAttention(s)).toEqual([]);
	});

	it('orders by severity and points at the screen that fixes each', () => {
		const rows = needsAttention(
			system({
				restart_required: ['network.dht_port', 'crawler.encryption'],
				diagnostics: { recent_errors: 12 },
				data_formats: { rebuild: { phase: 'failed' } }
			}),
			{ reachable: false } as never
		);
		expect(rows.map((r) => [r.kind, r.screen, r.count])).toEqual([
			['rebuild_failed', 'about', undefined],
			['unreachable', 'network', undefined],
			['restart', 'general', 2],
			['errors', 'logging', 12]
		]);
	});

	it('links a restart to the screen of the first changed setting', () => {
		const [row] = needsAttention(
			system({ restart_required: ['network.http_bind'] }),
			undefined,
			(section) => (section === 'network' ? 'network' : undefined)
		);
		expect(row).toMatchObject({ kind: 'restart', screen: 'network' });
	});

	it('reports a running rebuild as information only', () => {
		const [row] = needsAttention(system({ data_formats: { rebuild: { phase: 'building' } } }));
		expect(row).toMatchObject({ kind: 'rebuilding', severity: 'info' });
		expect(needsAttention(system({ data_formats: { rebuild: { phase: 'done' } } }))).toEqual([]);
	});
});
