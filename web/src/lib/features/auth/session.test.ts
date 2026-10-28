import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const nav = vi.hoisted(() => ({
	toSetup: vi.fn(),
	toLogin: vi.fn(),
	toLoginFrom: vi.fn()
}));
vi.mock('./nav', () => nav);

import { api } from '$lib/api/client';
import { cacheDetail, cachedDetail, onDetailInvalidated } from '$lib/features/detail/cache';
import { HASH, torrentDetail } from '$lib/features/detail/fixtures';
import { currentSession, installAuthHandlers, logout, setSession } from './session.svelte';

const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
const expired = (setup: boolean) =>
	json(401, { error: { code: 'login_required', message: 'Log in first.' }, setup_required: setup });

let uninstall: () => void;
beforeEach(() => {
	vi.clearAllMocks();
	history.replaceState(null, '', '/search?q=ubuntu&sort=size');
	setSession('signed_in');
	uninstall = installAuthHandlers();
});
afterEach(() => {
	uninstall();
	vi.unstubAllGlobals();
});

describe('installAuthHandlers', () => {
	it('does not repeat in-flight detail work when the initial session is confirmed', async () => {
		setSession('unknown');
		const invalidated = vi.fn();
		const off = onDetailInvalidated(invalidated);
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => json(200, { ok: true }))
		);
		try {
			await api.get('/filters');
			expect(currentSession()).toBe('signed_in');
			expect(invalidated).not.toHaveBeenCalled();
			setSession('signed_out');
			expect(invalidated).toHaveBeenLastCalledWith(false);
			await api.post('/login', { body: { password: 'x' } });
			expect(invalidated).toHaveBeenLastCalledWith(true);
			expect(invalidated).toHaveBeenCalledTimes(2);
		} finally {
			off();
		}
	});

	it('sends an expired session to sign-in with the current URL as next', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(false))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toLoginFrom).toHaveBeenCalledWith('/search?q=ubuntu&sort=size');
		expect(nav.toSetup).not.toHaveBeenCalled();
		expect(currentSession()).toBe('signed_out');
	});

	it('sends setup_required to /setup instead', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(true))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toSetup).toHaveBeenCalledOnce();
		expect(nav.toLoginFrom).not.toHaveBeenCalled();
		expect(currentSession()).toBe('setup_required');
	});

	it('marks the session signed in after any request that worked', async () => {
		setSession('unknown');
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => json(200, { ok: true }))
		);
		await api.post('/login', { body: { password: 'x' } });
		expect(currentSession()).toBe('signed_in');
	});

	it('uninstalls cleanly', async () => {
		uninstall();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(false))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toLoginFrom).not.toHaveBeenCalled();
		uninstall = installAuthHandlers();
	});
});

describe('logout', () => {
	it('invalidates detail state without revalidation on sign-out', () => {
		cacheDetail(HASH, torrentDetail());
		const invalidated = vi.fn();
		const off = onDetailInvalidated(invalidated);
		setSession('signed_out');
		expect(cachedDetail(HASH)).toBeUndefined();
		expect(invalidated).toHaveBeenCalledWith(false);
		setSession('signed_out');
		expect(invalidated).toHaveBeenCalledTimes(1);
		off();
	});

	it('posts JSON to /logout, then goes to sign-in', async () => {
		const fetch = vi.fn(async () => json(200, { ok: true }));
		vi.stubGlobal('fetch', fetch);
		await logout();
		const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
		expect(url).toBe('/api/v1/logout');
		expect(init.method).toBe('POST');
		expect((init.headers as Record<string, string>)['Content-Type']).toBe('application/json');
		expect(init.body).toBe('{}');
		expect(currentSession()).toBe('signed_out');
		expect(nav.toLogin).toHaveBeenCalledOnce();
	});

	it('stays put when the server is unreachable', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => Promise.reject(new TypeError('offline')))
		);
		await expect(logout()).rejects.toMatchObject({ code: 'network' });
		expect(nav.toLogin).not.toHaveBeenCalled();
		expect(currentSession()).toBe('signed_in');
	});
});
