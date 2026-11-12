import { describe, expect, it } from 'vitest';
import {
	EXPORT_URL,
	exportView,
	keepPolling,
	nameError,
	ourExport,
	suggestName,
	torznabKeys,
	torznabUrl,
	wider,
	type Job,
	type Key
} from './logic';

const key = (over: Partial<Key>): Key =>
	({
		id: 1,
		name: 'Prowlarr',
		kind: 'api_key',
		scopes: ['torznab'],
		status: 'active',
		prefix: 'ls_abcd',
		created_at: 1_700_000_000,
		last_used_at: null,
		...over
	}) as Key;

const job = (over: Partial<Job>): Job =>
	({
		id: 7,
		kind: 'export',
		state: 'running',
		started_at: 1_700_000_100,
		finished_at: null,
		done: 10,
		skipped: 2,
		total: 100,
		total_approx: false,
		rate: null,
		eta_s: null,
		export: { cap: 1_000_000, format: 'bitmagnet', truncated: false },
		...over
	}) as Job;

describe('Torznab address', () => {
	it("is the origin plus the path before the apps' own /api", () => {
		expect(torznabUrl('http://192.168.1.5:8191')).toBe('http://192.168.1.5:8191/api/v1/torznab');
		expect(torznabUrl('https://lodestar.example/')).toBe('https://lodestar.example/api/v1/torznab');
	});
});

describe('Torznab keys', () => {
	it('lists API keys with the torznab scope only', () => {
		const ks = [
			key({ id: 1 }),
			key({ id: 2, scopes: ['search'] }),
			key({ id: 3, scopes: ['search', 'torznab'] }),
			key({ id: 4, kind: 'feed_token', scopes: [] })
		];
		expect(torznabKeys(ks).map((k) => k.id)).toEqual([1, 3]);
		expect(wider(ks[0]!)).toBe(false);
		expect(wider(ks[2]!)).toBe(true);
	});

	it('suggests the first app no active key is named for', () => {
		expect(suggestName([])).toBe('Prowlarr');
		expect(suggestName([key({ name: 'Prowlarr' })])).toBe('Sonarr');
		// A revoked key's name is free again.
		expect(suggestName([key({ name: 'Prowlarr', status: 'revoked' })])).toBe('Prowlarr');
		const all = ['Prowlarr', 'Sonarr', 'Radarr', 'Lidarr', 'Readarr'].map((name) => key({ name }));
		expect(suggestName(all)).toBe('Prowlarr');
	});

	it('checks a name as the server does', () => {
		expect(nameError('  ')).toBe('required');
		expect(nameError('x'.repeat(200))).toBeNull();
		expect(nameError('x'.repeat(201))).toBe('long');
		// Characters, not UTF-16 units.
		expect(nameError('😀'.repeat(200))).toBeNull();
	});
});

describe('bitmagnet export', () => {
	it('asks for the bitmagnet format and nothing else', () => {
		expect(EXPORT_URL).toBe('/api/v1/export?format=bitmagnet');
		expect(EXPORT_URL).not.toContain('hidden');
	});

	it('finds the export started at or after the click, newest first', () => {
		const jobs = [
			job({ id: 9, kind: 'purge', started_at: 1_700_000_200, export: undefined }),
			job({
				id: 8,
				started_at: 1_700_000_150,
				export: { cap: 1, format: 'csv', truncated: false }
			}),
			job({ id: 7, started_at: 1_700_000_100 }),
			job({ id: 6, started_at: 1_699_000_000 })
		];
		expect(ourExport(jobs, 1_700_000_100)?.id).toBe(7);
		expect(ourExport(jobs, 1_700_000_103)?.id).toBe(7);
		expect(ourExport(jobs, 1_700_000_200)).toBeNull();
		expect(ourExport([], 1)).toBeNull();
	});

	it('shows the server word for each state', () => {
		expect(exportView(null)).toEqual({ state: 'waiting' });
		expect(exportView(job({}))).toEqual({ state: 'running', done: 10, total: 100, approx: false });
		expect(exportView(job({ state: 'paused', total: null }))).toEqual({
			state: 'paused',
			done: 10,
			total: null,
			approx: false
		});
		expect(
			exportView(
				job({
					state: 'done',
					finished_at: 1,
					export: { cap: 1_000_000, format: 'bitmagnet', truncated: true }
				})
			)
		).toEqual({ state: 'done', done: 10, skipped: 2, truncated: true, cap: 1_000_000 });
		expect(exportView(job({ state: 'failed', finished_at: 1 }))).toEqual({
			state: 'failed',
			done: 10
		});
		expect(exportView(job({ state: 'cancelled', finished_at: 1 }))).toEqual({
			state: 'cancelled',
			done: 10
		});
	});

	it('polls until the job finishes, or gives up when none appears', () => {
		expect(keepPolling(job({}), 100)).toBe(true);
		expect(keepPolling(job({ finished_at: 5 }), 0)).toBe(false);
		expect(keepPolling(null, 29)).toBe(true);
		expect(keepPolling(null, 30)).toBe(false);
	});
});
