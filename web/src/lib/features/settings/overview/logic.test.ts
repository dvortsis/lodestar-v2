import { afterEach, describe, expect, it } from 'vitest';
import { STEPS, writeProgress } from '$lib/features/wizard/steps';
import type { components } from '$lib/api/schema';
import type { FieldKey, FieldSpec, SectionValues } from '../form/types';
import {
	changedFromDefault,
	needsAttention,
	retryIn,
	skippedSteps,
	startHere,
	STEP_SCREEN,
	startReason,
	type StartFailure
} from './logic';

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

	describe('workers that cannot start (X-cluster-start-retry)', () => {
		type Workers = components['schemas']['WorkerList'];
		const failure: StartFailure = {
			step: 'bind',
			kind: 'addr_in_use',
			next_retry_at: 1_700_000_120
		};
		const workers = (start_failure: StartFailure | null, allowed = false): Workers => ({
			allowed,
			start_failure,
			workers: []
		});

		it('shows a warning with no link while the port is being tried again', () => {
			const rows = needsAttention(system(), undefined, undefined, workers(failure));
			expect(rows).toEqual([
				{ kind: 'workers_start_failing', severity: 'warning', startFailure: failure }
			]);
			expect(rows[0]!.screen).toBeUndefined();
		});

		it('links to the logs when the fix is not in config.toml', () => {
			const store: StartFailure = { step: 'store', kind: 'store', next_retry_at: 0 };
			const [row] = needsAttention(system(), undefined, undefined, workers(store));
			expect(row).toMatchObject({ kind: 'workers_start_failing', href: '/logs' });
			const bind: StartFailure = { step: 'bind', kind: 'io', next_retry_at: 0 };
			expect(needsAttention(system(), undefined, undefined, workers(bind))[0]!.href).toBe('/logs');
			for (const kind of ['addr_in_use', 'addr_not_available', 'permission_denied'] as const) {
				const own: StartFailure = { step: 'bind', kind, next_retry_at: 0 };
				expect(
					needsAttention(system(), undefined, undefined, workers(own))[0]!.href
				).toBeUndefined();
			}
		});

		it('goes away once the port opens', () => {
			expect(needsAttention(system(), undefined, undefined, workers(null, true))).toEqual([]);
		});

		it('says nothing when workers are off or the list is unavailable', () => {
			expect(needsAttention(system(), undefined, undefined, workers(null))).toEqual([]);
			expect(needsAttention(system(), undefined, undefined, undefined)).toEqual([]);
		});

		it('sorts among the other warnings, after errors', () => {
			const rows = needsAttention(
				system({ data_formats: { rebuild: { phase: 'failed' } } }),
				undefined,
				undefined,
				workers(failure)
			);
			expect(rows.map((r) => r.kind)).toEqual(['rebuild_failed', 'workers_start_failing']);
		});

		it('says when it tries next: minutes rounded up, seconds under a minute, else shortly', () => {
			expect(retryIn(1000, 1000)).toBeNull();
			expect(retryIn(1044, 1000)).toBeNull();
			expect(retryIn(990, 1000)).toBeNull(); // already due
			expect(retryIn(1050, 1000)).toEqual({ value: 50, unit: 'second' });
			expect(retryIn(1060, 1000)).toEqual({ value: 1, unit: 'minute' });
			expect(retryIn(1150, 1000)).toEqual({ value: 3, unit: 'minute' });
			expect(retryIn(1300, 1000)).toEqual({ value: 5, unit: 'minute' });
		});

		it('names the reason in plain words: the kind where it says more, else the step', () => {
			const f = (step: StartFailure['step'], kind: StartFailure['kind']) =>
				startReason({ step, kind, next_retry_at: 0 });
			expect(f('bind', 'addr_in_use')).toBe('port_in_use');
			expect(f('bind', 'addr_not_available')).toBe('address');
			expect(f('bind', 'permission_denied')).toBe('permission');
			expect(f('bind', 'io')).toBe('port');
			expect(f('store', 'store')).toBe('database');
			expect(f('pauses', 'store')).toBe('database');
			expect(f('secrets', 'secrets')).toBe('secrets');
			expect(f('pki', 'corrupt')).toBe('certificates');
			expect(f('pki', 'task_failed')).toBe('certificates');
			expect(f('server', 'tls')).toBe('server');
		});
	});

	it('reports a running rebuild as information only', () => {
		const [row] = needsAttention(system({ data_formats: { rebuild: { phase: 'building' } } }));
		expect(row).toMatchObject({ kind: 'rebuilding', severity: 'info' });
		expect(needsAttention(system({ data_formats: { rebuild: { phase: 'done' } } }))).toEqual([]);
	});
});

describe('startHere', () => {
	const none: SectionValues[] = [];
	it('lists the five basics in order, with no state where the reads say nothing', () => {
		const rows = startHere(none, []);
		expect(rows.map((r) => r.id)).toEqual([
			'client',
			'notifications',
			'updates',
			'backups',
			'password'
		]);
		expect(rows.map((r) => r.state)).toEqual(['none', 'none', 'off', undefined, undefined]);
		expect(rows.map((r) => r.screen)).toEqual([
			'integrations',
			'notifications',
			'updates',
			'general',
			'security'
		]);
	});

	it('reads the state from the values already loaded', () => {
		const rows = startHere(
			[
				section('integrations', { clients: v([{ id: 'a' }, { id: 'b' }], 'db') }),
				section('notifications', { ntfy_url: v('https://ntfy.sh/x', 'db') }),
				section('updates', { mode: v('notify', 'db') })
			],
			[]
		);
		expect(rows[0]).toMatchObject({ state: 'set', count: 2, configured: true });
		expect(rows[1]).toMatchObject({ state: 'set', configured: true });
		expect(rows[2]).toMatchObject({ state: 'set', configured: true });
	});

	it('treats an update mode of off, or a default, as off', () => {
		expect(startHere([section('updates', { mode: v('off', 'db') })], [])[2]!.state).toBe('off');
		expect(startHere([section('updates', { mode: v('notify') })], [])[2]!.state).toBe('off');
	});

	it('adds each skipped first-run step the basics do not cover, once', () => {
		const rows = startHere(none, [
			{ step: 'download', screen: 'integrations' },
			{ step: 'storage', screen: 'storage' },
			{ step: 'packs', screen: 'filters' },
			{ step: 'blocklists', screen: 'filters' }
		]);
		expect(rows.slice(5).map((r) => [r.id, r.screen, r.state])).toEqual([
			['skipped-storage', 'storage', 'skipped'],
			['skipped-packs', 'filters', 'skipped']
		]);
	});
});

describe('skippedSteps', () => {
	afterEach(() => sessionStorage.clear());
	it('names a Settings screen for exactly the optional steps the wizard has', () => {
		const optional = STEPS.filter((s) => s.optional);
		expect(Object.keys(STEP_SCREEN).sort()).toEqual(optional.map((s) => s.id).sort());
		for (const s of optional) expect(STEP_SCREEN[s.id]).toBe(s.settings);
	});
	it('reads the skipped steps from the wizard progress, and nothing when there is none', () => {
		expect(skippedSteps()).toEqual([]);
		writeProgress({ step: 'done', skipped: ['download', 'packs'], done: [] });
		expect(skippedSteps()).toEqual([
			{ step: 'download', screen: 'integrations' },
			{ step: 'packs', screen: 'filters' }
		]);
		sessionStorage.setItem('lodestar.wizard', '{not json');
		expect(skippedSteps()).toEqual([]);
	});
});
