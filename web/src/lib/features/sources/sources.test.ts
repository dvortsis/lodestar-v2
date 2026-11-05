import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import { dht, nyaa, nyaaAcked, prowlarr, source } from './fixtures';
import { group, refusalText, setupNeed, statusOf } from './sources';

describe('grouping', () => {
	it('groups by first known kind in kind order, rows by name, and skips empty groups', () => {
		const imp = source({ id: 3, name: 'Alpha', kinds: ['importer'] });
		const odd = source({ id: 4, name: 'Odd', kinds: [] });
		const g = group([prowlarr, odd, nyaa, imp, dht]);
		expect(g.map((x) => [x.kind, x.sources.map((s) => s.name)])).toEqual([
			['discovery', ['Mainline DHT', 'Nyaa']],
			['importer', ['Alpha', 'Prowlarr']],
			[null, ['Odd']]
		]);
	});
});

describe('status', () => {
	it('derives one status per row state', () => {
		expect(statusOf(dht)).toBe('running');
		expect(statusOf(nyaa)).toBe('needs_setup');
		expect(statusOf(prowlarr)).toBe('error');
		expect(statusOf(source({ id: 5, name: 'a', enabled: true, state: 'enabled' }))).toBe('waiting');
		expect(statusOf(source({ id: 5, name: 'a', state: 'paused' }))).toBe('paused');
		expect(statusOf(source({ id: 5, name: 'a' }))).toBe('off');
		expect(statusOf(source({ id: 5, name: 'a', state: 'purged' }))).toBe('purged');
		expect(statusOf(source({ id: 5, name: 'a', purge_pending: true }))).toBe('purge_pending');
	});

	it('names what setup a disabled source still needs, from the API `needs`', () => {
		expect(setupNeed(nyaa)).toBe('endpoints');
		expect(setupNeed(source({ id: 6, name: 'a', needs: 'credentials' }))).toBe('credentials');
		// `needs` is the server's word: stored credentials do not clear it here.
		expect(setupNeed(source({ id: 6, name: 'a', needs: 'credentials', credentials: ['k'] }))).toBe(
			'credentials'
		);
		expect(setupNeed(source({ id: 6, name: 'a', needs: 'consent', consent_required: true }))).toBe(
			'consent'
		);
		expect(setupNeed(source({ id: 6, name: 'a', password_required: true }))).toBe('password');
		expect(setupNeed(dht)).toBeNull();
	});

	it('lets a source acknowledged earlier toggle back on, though its class still needs consent', () => {
		expect(nyaaAcked.consent_required).toBe(true);
		expect(setupNeed(nyaaAcked)).toBeNull();
		expect(statusOf(nyaaAcked)).toBe('off');
	});
});

describe('refusals', () => {
	it('reads the gate codes plainly and falls back to the server text', () => {
		expect(refusalText(new ApiError(409, 'consent_required', 'x'))).toContain('needs setup');
		expect(refusalText(new ApiError(409, 'purge_running', 'x'))).toContain('purge');
		expect(refusalText(new ApiError(400, 'other', 'Server words'))).toContain('(other)');
		expect(refusalText(new Error('x'))).toContain('Nothing was changed');
	});
});
