import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const region = vi.hoisted(() => ({ refetchRegion: vi.fn() }));
vi.mock('$lib/i18n/boot', () => region);

const nav = vi.hoisted(() => ({
	toSetup: vi.fn(),
	toLogin: vi.fn(),
	toLoginFrom: vi.fn()
}));
vi.mock('./nav', () => nav);

import { api } from '$lib/api/client';
import { cacheDetail, cachedDetail, onDetailInvalidated } from '$lib/features/detail/cache';
import { HASH, torrentDetail } from '$lib/features/detail/fixtures';
import {
	clearSettingsDrafts,
	currentSession,
	installAuthHandlers,
	logout,
	rearmSignIn,
	REARM_EVENT,
	SETTINGS_DRAFTS_PREFIX,
	setSession,
	SIGN_IN_KEY,
	signedIn,
	signInPending
} from './session.svelte';
import { checkAuth } from './gate';
import * as scope from '$lib/features/settings/form/draft-scope';
import { writeDraft } from '$lib/features/settings/form/draft-store';

const { DRAFT_PREFIX, KEPT_KEY } = scope;
const markKept = (id: string, path: string, at: number) =>
	sessionStorage.setItem(KEPT_KEY, JSON.stringify({ id, path, at }));

const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
const expired = (setup: boolean) =>
	json(401, { error: { code: 'login_required', message: 'Log in first.' }, setup_required: setup });

let uninstall: () => void;
beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	history.replaceState(null, '', '/search?q=ubuntu&sort=size');
	setSession('signed_in');
	uninstall = installAuthHandlers();
});
afterEach(() => {
	uninstall();
	vi.unstubAllGlobals();
});

describe('region refetch on sign-in', () => {
	it('refetches when the status becomes signed in after being signed out', async () => {
		setSession('signed_out');
		region.refetchRegion.mockClear();
		setSession('signed_in');
		await vi.waitFor(() => expect(region.refetchRegion).toHaveBeenCalledTimes(1));
	});
	it('refetches from unknown on the login page (a failed GET /auth), not on a normal load', async () => {
		setSession('unknown');
		region.refetchRegion.mockClear();
		history.replaceState(null, '', '/search');
		setSession('signed_in');
		await Promise.resolve();
		await Promise.resolve();
		expect(region.refetchRegion).not.toHaveBeenCalled();
		setSession('unknown');
		history.replaceState(null, '', '/login?next=%2Fsearch');
		setSession('signed_in');
		await vi.waitFor(() => expect(region.refetchRegion).toHaveBeenCalledTimes(1));
	});
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

	it('sends a pending Legal Notice to the sign-in page, keeping the session', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () =>
				json(409, { error: { code: 'notice_required', message: 'Read it first.' } })
			)
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toLoginFrom).toHaveBeenCalledWith('/search?q=ubuntu&sort=size');
		expect(currentSession()).toBe('signed_in');
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

	it('does not invalidate the detail caches on the first answer, only on a later change', () => {
		setSession('unknown');
		const seen = vi.fn();
		const off = onDetailInvalidated(seen);
		setSession('signed_in');
		expect(seen).not.toHaveBeenCalled();
		setSession('signed_out');
		setSession('signed_in');
		expect(seen).toHaveBeenLastCalledWith(true);
		off();
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

describe('Settings drafts across the session (X-session-draft-keep)', () => {
	const seed = () => {
		sessionStorage.clear();
		sessionStorage.setItem(DRAFT_PREFIX + 'crawler', '{"v":1}');
		markKept('crawler', '/settings/crawler', Date.now());
	};
	const stored = () => [DRAFT_PREFIX + 'crawler', KEPT_KEY].map((k) => sessionStorage.getItem(k));

	it('keeps them when the session expires, and says a sign-in is pending', async () => {
		seed();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(false))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(signInPending()).toBe('expired');
		expect(stored().every((v) => v !== null)).toBe(true);
		setSession('signed_in');
		expect(signInPending()).toBeNull();
	});

	it('keeps them for the Legal Notice, which keeps the session', async () => {
		seed();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () =>
				json(409, { error: { code: 'notice_required', message: 'Read it first.' } })
			)
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(signInPending()).toBe('notice');
		expect(stored().every((v) => v !== null)).toBe(true);
	});

	it('drops them all on Sign out, and blocks new ones until the next sign-in', async () => {
		seed();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => json(200, { ok: true }))
		);
		await logout();
		expect(stored()).toEqual([null, null]);
		expect(signInPending()).toBe('signedout');
		// A pending debounce or a form torn down later can't write one back.
		expect(writeDraft('crawler', { v: 1, at: 1, sections: {} })).toBe(false);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'crawler')).toBeNull();
		// A save refused afterwards is no expiry: nothing is kept for it.
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(false))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(signInPending()).toBe('signedout');
		// Choosing to stay re-arms the redirect, never the block.
		rearmSignIn();
		expect(signInPending()).toBe('signedout');
		// Only a real sign-in lifts it.
		signedIn();
		expect(signInPending()).toBeNull();
		expect(writeDraft('crawler', { v: 1, at: 1, sections: {} })).toBe(true);
	});

	it('a late success after Sign out keeps drafts blocked', async () => {
		seed();
		// A preview sent just before Sign out answers just after it.
		let answer!: (r: Response) => void;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string | URL | Request) =>
				String(url).includes('/logout')
					? json(200, { ok: true })
					: new Promise<Response>((r) => (answer = r))
			)
		);
		const late = api.get('/filters');
		await logout();
		answer(json(200, []));
		await late.catch(() => {});
		expect(signInPending()).toBe('signedout');
		expect(writeDraft('crawler', { v: 1, at: 1, sections: {} })).toBe(false);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'crawler')).toBeNull();
	});

	it('drops them all when the instance needs its first-run setup again', async () => {
		seed();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(true))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(stored()).toEqual([null, null]);
		expect(signInPending()).toBe('setup');
		// A late success doesn't lift the block; the setup or a sign-in does.
		setSession('signed_in');
		expect(writeDraft('crawler', { v: 1, at: 1, sections: {} })).toBe(false);
		signedIn();
		expect(writeDraft('crawler', { v: 1, at: 1, sections: {} })).toBe(true);
	});

	it('drops them all when GET /auth finds the instance re-claimed (a reload)', async () => {
		seed();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () =>
				json(200, { setup_required: true, logged_in: false, notice_required: false })
			)
		);
		await checkAuth();
		expect(stored()).toEqual([null, null]);
		expect(signInPending()).toBe('setup');
		expect(writeDraft('crawler', { v: 1, at: 1, sections: {} })).toBe(false);
	});

	it('drops them all whenever the status becomes setup_required, however it is learned', () => {
		seed();
		setSession('setup_required');
		expect(stored()).toEqual([null, null]);
		seed();
		setSession('setup_required'); // already the status: still dropped
		expect(stored()).toEqual([null, null]);
	});

	it('redirects again after the user chose to stay', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(false))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toLoginFrom).toHaveBeenCalledTimes(1); // once until a request works
		rearmSignIn();
		expect(signInPending()).toBeNull();
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toLoginFrom).toHaveBeenCalledTimes(2);
	});
});

describe('the keys the shell and the Settings form share (no import between them)', () => {
	it('match', () => {
		expect(scope.SIGN_IN_KEY).toBe(SIGN_IN_KEY);
		expect(scope.REARM_EVENT).toBe(REARM_EVENT);
		expect(DRAFT_PREFIX.startsWith(SETTINGS_DRAFTS_PREFIX)).toBe(true);
		expect(KEPT_KEY.startsWith(SETTINGS_DRAFTS_PREFIX)).toBe(true);
		expect(SIGN_IN_KEY.startsWith(SETTINGS_DRAFTS_PREFIX)).toBe(false); // a draft clear keeps it
	});

	it('the form sees what the shell wrote, and its re-arm reaches the shell', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => expired(false))
		);
		await expect(api.get('/filters')).rejects.toThrow();
		expect(scope.signInPending()).toBe('expired');
		dispatchEvent(new Event(scope.REARM_EVENT)); // what signed-out.ts sends
		expect(signInPending()).toBeNull();
		await expect(api.get('/filters')).rejects.toThrow();
		expect(nav.toLoginFrom).toHaveBeenCalledTimes(2);
	});

	it('clearSettingsDrafts drops every draft and the marker, and nothing else', () => {
		sessionStorage.clear();
		sessionStorage.setItem(DRAFT_PREFIX + 'crawler', '{}');
		sessionStorage.setItem(DRAFT_PREFIX + 'notifications', '{}');
		markKept('crawler', '/settings/crawler', 1);
		sessionStorage.setItem('lodestar.other', 'kept');
		clearSettingsDrafts();
		expect(sessionStorage.length).toBe(1);
		expect(sessionStorage.getItem('lodestar.other')).toBe('kept');
	});
});
