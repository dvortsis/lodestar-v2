import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import {
	EXPORT_FILE,
	MAX_IMPORT_BYTES,
	applyImport,
	dryRun,
	exportToml,
	skipText,
	undoBatch,
	type ImportReport
} from './backup';
import { savedSettings } from './reset';
import { goldenView, settingsApi } from '../shell/testing';

beforeEach(() => vi.spyOn(console, 'warn').mockImplementation(() => {}));
afterEach(() => vi.unstubAllGlobals());

const report = (over: Partial<ImportReport> = {}): ImportReport => ({
	applied: true,
	changes: [],
	revisions: {},
	skipped: [],
	unchanged: 0,
	...over
});

describe('undoBatch', () => {
	it('sends each change’s from where it came from the app, null otherwise, with the revisions after the import', () => {
		expect(
			undoBatch(
				report({
					revisions: { crawler: 5, network: 3 },
					changes: [
						{
							section: 'crawler',
							field: 'intensity',
							from: 'balanced',
							from_source: 'db',
							restart_required: false,
							to: 'low'
						},
						{
							section: 'crawler',
							field: 'timeout_secs',
							from: 10,
							from_source: 'default',
							restart_required: true,
							to: 30
						},
						{
							section: 'network',
							field: 'dht_port',
							from: 6881,
							from_source: 'file',
							restart_required: true,
							to: 7000
						}
					]
				})
			)
		).toEqual({
			crawler: { if_revision: 5, intensity: 'balanced', timeout_secs: null },
			network: { if_revision: 3, dht_port: null }
		});
	});

	it('has nothing to send for no changes', () => {
		expect(undoBatch(report())).toEqual({});
	});
});

describe('skipText', () => {
	it.each([
		[{ key: 'x.y', reason: 'unknown', env: null }, 'x.y: no such setting'],
		[{ key: 'general.data_dir', reason: 'read_only', env: null }, 'general.data_dir: read-only'],
		[
			{ key: 'secrets.token', reason: 'secret', env: null },
			'secrets.token: secrets are never imported'
		],
		[
			{ key: 'network.dht_port', reason: 'set_by_environment', env: 'LODESTAR_DHT_PORT' },
			'network.dht_port: set by LODESTAR_DHT_PORT'
		],
		[{ key: 'a.b', reason: 'brand_new', env: null }, 'a.b: left alone']
	])('words %j', (skip, text) => {
		expect(skipText(skip)).toBe(text);
	});
});

describe('requests', () => {
	it('exports as text, with or without preferences', async () => {
		const api = settingsApi({
			'GET /settings/export?preferences=false': () => ({ status: 200, body: 'ignored' })
		});
		const fetch = vi.fn(async () => new Response('dht.intensity = "low"\n', { status: 200 }));
		vi.stubGlobal('fetch', fetch);
		expect(await exportToml(true)).toBe('dht.intensity = "low"\n');
		expect(fetch).toHaveBeenCalledWith('/api/v1/settings/export?preferences=true', {
			headers: { Accept: 'application/toml' },
			credentials: 'same-origin'
		});
		void api;
	});

	it('turns a failed export into the server’s error', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () =>
				Response.json({ error: { code: 'internal', message: 'disk' } }, { status: 500 })
			)
		);
		await expect(exportToml(false)).rejects.toMatchObject({ code: 'internal', status: 500 });
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => Promise.reject(new TypeError('offline')))
		);
		await expect(exportToml(false)).rejects.toBeInstanceOf(ApiError);
	});

	it('sends the dry run with ?dry_run=true and the apply with the revisions, and confirm_adult only when asked', async () => {
		const api = settingsApi({
			'POST /settings/import?dry_run=true': () => ({
				status: 200,
				body: report({ applied: false })
			}),
			'POST /settings/import': () => ({ status: 200, body: report() })
		});
		await dryRun('a = 1');
		await applyImport('a = 1', { crawler: 3 });
		await applyImport('a = 1', { crawler: 3 }, true);
		expect(
			api.calls.filter((c) => c.path.startsWith('/settings/import')).map((c) => [c.path, c.body])
		).toEqual([
			['/settings/import?dry_run=true', { toml: 'a = 1' }],
			['/settings/import', { toml: 'a = 1', revisions: { crawler: 3 } }],
			['/settings/import', { toml: 'a = 1', revisions: { crawler: 3 }, confirm_adult: true }]
		]);
	});

	it('keeps the limits the server has', () => {
		expect(MAX_IMPORT_BYTES).toBe(65536);
		expect(EXPORT_FILE).toBe('lodestar-settings.toml');
	});
});

describe('savedSettings (Reset all)', () => {
	const sec = (section: string, revision: number, fields: Record<string, unknown>) => ({
		section,
		revision,
		restart_required: [],
		fields
	});
	const f = (source: string, over: Record<string, unknown> = {}) => ({
		value: 1,
		source,
		env: null,
		read_only: false,
		restart_required: false,
		...over
	});

	it('counts the values saved in the app outside Preferences, and builds one batch with each revision', async () => {
		const view = goldenView();
		settingsApi({
			'GET /settings/crawler': () => ({
				status: 200,
				body: sec('crawler', 4, {
					intensity: f('db'),
					paused: f('default'),
					timeout_secs: f('db'),
					encryption: f('file')
				})
			}),
			'GET /settings/network': () => ({
				status: 200,
				body: sec('network', 2, { dht_port: f('env'), http_bind: f('db', { read_only: true }) })
			}),
			'GET /settings/preferences': () => ({
				status: 200,
				body: sec('preferences', 9, { theme: f('db') })
			})
		});
		const saved = await savedSettings(view);
		expect(saved.count).toBe(2);
		expect(saved.batch).toEqual({
			crawler: { if_revision: 4, intensity: null, timeout_secs: null }
		});
	});
});
