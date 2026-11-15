import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const goto = vi.hoisted(() => vi.fn());
const toSetup = vi.hoisted(() => vi.fn());
const setSession = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto }));
vi.mock('./nav', () => ({ toSetup }));
vi.mock('./session.svelte', () => ({ setSession }));

import { checkFirstRun } from './first-check';
import { firstRunTarget, leaveForFirstRun } from './first-run';

const status = (logged_in: boolean, notice_required: boolean, setup_required: boolean) => ({
	logged_in,
	notice_required,
	setup_required
});

const fetchMock = vi.fn();
const answer = (body: unknown, ok = true) =>
	fetchMock.mockResolvedValueOnce({ ok, json: () => Promise.resolve(body) });

beforeEach(() => {
	for (const f of [goto, toSetup, setSession, fetchMock]) f.mockReset();
	vi.stubGlobal('fetch', fetchMock);
	history.replaceState(null, '', '/search?q=ubuntu');
});
afterEach(() => vi.unstubAllGlobals());

describe('firstRunTarget', () => {
	it('sends a fresh instance to setup, whatever else it says', () => {
		expect(firstRunTarget(status(false, false, true))).toBe('setup');
		expect(firstRunTarget(status(false, true, true))).toBe('setup');
	});

	it('sends a signed-out visitor or a pending Legal Notice to sign-in', () => {
		expect(firstRunTarget(status(false, false, false))).toBe('login');
		expect(firstRunTarget(status(true, true, false))).toBe('login');
	});

	it('leaves a signed-in visitor with the notice accepted where they are', () => {
		expect(firstRunTarget(status(true, false, false))).toBeNull();
	});
});

describe('leaveForFirstRun', () => {
	it('goes to setup on a fresh instance', async () => {
		await leaveForFirstRun(status(false, false, true));
		expect(setSession).toHaveBeenCalledWith('setup_required');
		expect(toSetup).toHaveBeenCalledOnce();
		expect(goto).not.toHaveBeenCalled();
	});

	it('goes to sign-in with the current page as next, replacing the entry', async () => {
		await leaveForFirstRun(status(false, false, false));
		expect(setSession).toHaveBeenCalledWith('signed_out');
		expect(goto).toHaveBeenCalledExactlyOnceWith('/login?next=%2Fsearch%3Fq%3Dubuntu', {
			replaceState: true
		});
	});

	it('keeps the session signed in when only the notice is pending', async () => {
		await leaveForFirstRun(status(true, true, false));
		expect(setSession).not.toHaveBeenCalled();
		expect(goto).toHaveBeenCalledOnce();
	});
});

describe('checkFirstRun', () => {
	it('stays when the visitor has moved on by the time the answer comes', async () => {
		answer(status(false, false, true));
		await checkFirstRun(() => false);
		expect(toSetup).not.toHaveBeenCalled();
		expect(goto).not.toHaveBeenCalled();
	});

	it('asks GET /auth once and leaves when it must', async () => {
		answer(status(false, false, true));
		await checkFirstRun(() => true);
		expect(fetchMock).toHaveBeenCalledExactlyOnceWith('/api/v1/auth', expect.any(Object));
		expect(toSetup).toHaveBeenCalledOnce();
	});

	it('stays when signed in, when the answer is not OK, and when the request fails', async () => {
		answer(status(true, false, false));
		await checkFirstRun(() => true);
		answer({}, false);
		await checkFirstRun(() => true);
		fetchMock.mockRejectedValueOnce(new Error('offline'));
		await checkFirstRun(() => true);
		expect(goto).not.toHaveBeenCalled();
		expect(toSetup).not.toHaveBeenCalled();
		expect(setSession).not.toHaveBeenCalled();
	});
});
