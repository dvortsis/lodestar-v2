import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import { dht, nyaa, nyaaAcked, prowlarr, source } from './fixtures';
import {
	CONSENT_CLASSES,
	consentHelp,
	consentName,
	group,
	matches,
	needsAttention,
	refusalText,
	setupNeed,
	statusOf,
	summary
} from './sources';

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

describe('filter, attention and summary (X-sources-restructure)', () => {
	it('matches by name or by kind, ignoring case, and an empty query matches all', () => {
		expect(matches(nyaa, '')).toBe(true);
		expect(matches(nyaa, '  ')).toBe(true);
		expect(matches(nyaa, 'NYA')).toBe(true);
		expect(matches(prowlarr, 'importers')).toBe(true);
		// The DHT is listed under Discovery but is also a peer finder.
		expect(matches(dht, 'peer find')).toBe(true);
		expect(matches(nyaa, 'peer find')).toBe(false);
		expect(matches(source({ id: 4, name: 'Odd', kinds: [] }), 'other')).toBe(true);
		expect(matches(nyaa, 'prowl')).toBe(false);
	});

	it('flags an error, a pending restart or a waiting companion, not a source that is off', () => {
		expect(needsAttention(dht)).toBe(false);
		expect(needsAttention(nyaa)).toBe(false); // off, needs setup: not attention
		expect(needsAttention(prowlarr)).toBe(true); // error and restart
		expect(needsAttention(source({ id: 5, name: 'R', restart_required: true }))).toBe(true);
		expect(needsAttention(source({ id: 7, name: 'P', purge_pending: true }))).toBe(true);
		const waiting = source({
			id: 6,
			name: 'W',
			enabled: true,
			state: 'enabled',
			companion_need: nyaa.companion_need ?? ({} as never)
		});
		expect(needsAttention(waiting)).toBe(true);
	});

	it('summarises a group: on, total and attention', () => {
		expect(summary([dht, nyaa, prowlarr])).toEqual({ on: 2, total: 3, attention: 1 });
		expect(summary([nyaa])).toEqual({ on: 0, total: 1, attention: 0 });
	});

	it('names every consent class, Open for none, with help text', () => {
		expect(CONSENT_CLASSES.map(consentName)).toEqual([
			'Open',
			'External',
			'Account',
			'Restricted',
			'Overlay'
		]);
		for (const c of CONSENT_CLASSES) expect(consentHelp(c).length).toBeGreaterThan(10);
	});
});

describe('Account and the password (X-consent-gate-lift r2)', () => {
	it('a disabled Account source with its key stored needs no separate password step', () => {
		// r2 makes `password_required` true for Account; the Account dialog asks for it, so the
		// row shows no "needs your password" note and no Needs setup status.
		const stored = { ...prowlarr, enabled: false, state: 'disabled' as const, needs: null };
		for (const password_required of [false, true]) {
			const s = { ...stored, password_required, restart_required: false };
			expect(setupNeed(s)).toBeNull();
			expect(statusOf(s)).toBe('off');
		}
		// Its missing key is still a setup step.
		expect(setupNeed({ ...stored, password_required: true, needs: 'credentials' })).toBe(
			'credentials'
		);
		// Restricted and Overlay still ask for the password as their own step.
		expect(setupNeed({ ...stored, consent: 'restricted', password_required: true })).toBe(
			'password'
		);
	});
});
