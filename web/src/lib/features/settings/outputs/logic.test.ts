import { describe, expect, it } from 'vitest';
import {
	EXPORT_URL,
	exportView,
	keepPolling,
	nameError,
	lastExportId,
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

	it('finds the export by id, whatever either clock says', () => {
		const old = job({
			id: 6,
			state: 'done',
			finished_at: 1_700_000_090,
			started_at: 1_700_000_080
		});
		expect(lastExportId([old, job({ id: 9, kind: 'purge', export: undefined })])).toBe(6);
		expect(lastExportId([])).toBe(0);
		// The browser's clock runs ahead of the server's: the new job "started" before the click.
		const ahead = [job({ id: 7, started_at: 1_700_000_000 }), old];
		expect(ourExport(ahead, 6, 1_700_000_500)?.id).toBe(7);
		// It runs behind: an export that finished just before the click looks newer than it, but
		// its id isn't, so its "Done" is never shown for the new download.
		expect(ourExport([old], 6, 1_699_999_000)).toBeNull();
		// Newest first; another format's export doesn't count.
		const two = [
			job({ id: 9, export: { cap: 1, format: 'csv', truncated: false } }),
			job({ id: 8 }),
			job({ id: 7 })
		];
		expect(ourExport(two, 6, 0)?.id).toBe(8);
	});

	it('waits while the newest export above the id has no format yet, never taking an older one', () => {
		// A double press: the second press's job (8) has just started and has no format yet; the
		// first press's (7) mustn't stand in for it.
		const jobs = [job({ id: 8, export: undefined }), job({ id: 7 })];
		expect(ourExport(jobs, 6, 0)).toBeNull();
		expect(ourExport([job({ id: 8 }), job({ id: 7 })], 6, 0)?.id).toBe(8);
	});

	it('falls back to the start time when no answer came before the click', () => {
		const jobs = [
			job({ id: 7, started_at: 1_700_000_100 }),
			job({ id: 6, started_at: 1_699_000_000 })
		];
		expect(ourExport(jobs, null, 1_700_000_100)?.id).toBe(7);
		expect(ourExport(jobs, null, 1_700_000_103)?.id).toBe(7);
		expect(ourExport(jobs, null, 1_700_000_200)).toBeNull();
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
