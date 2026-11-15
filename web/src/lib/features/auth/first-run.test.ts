import { beforeEach, describe, expect, it, vi } from 'vitest';

const goto = vi.hoisted(() => vi.fn());
const get = vi.hoisted(() => vi.fn());
const toSetup = vi.hoisted(() => vi.fn());
const setSession = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto }));
vi.mock('$lib/api/client', () => ({ api: { get } }));
vi.mock('./nav', () => ({ toSetup }));
vi.mock('./session.svelte', () => ({ setSession }));

import { firstRunTarget, gateFirstRun } from './first-run';

const status = (logged_in: boolean, notice_required: boolean, setup_required: boolean) => ({
	logged_in,
	notice_required,
	setup_required
});

beforeEach(() => {
	for (const f of [goto, get, toSetup, setSession]) f.mockReset();
	history.replaceState(null, '', '/search?q=ubuntu');
});

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

describe('gateFirstRun', () => {
	it('goes to setup on a fresh instance', async () => {
		get.mockResolvedValue(status(false, false, true));
		await gateFirstRun();
		expect(get).toHaveBeenCalledExactlyOnceWith('/auth');
		expect(setSession).toHaveBeenCalledWith('setup_required');
		expect(toSetup).toHaveBeenCalledOnce();
		expect(goto).not.toHaveBeenCalled();
	});

	it('goes to sign-in with the current page as next, replacing the entry', async () => {
		get.mockResolvedValue(status(false, false, false));
		await gateFirstRun();
		expect(setSession).toHaveBeenCalledWith('signed_out');
		expect(goto).toHaveBeenCalledExactlyOnceWith(
			expect.stringMatching(/^\/login\?next=%2Fsearch%3Fq%3Dubuntu$/),
			{ replaceState: true }
		);
	});

	it('keeps the session signed in when only the notice is pending', async () => {
		get.mockResolvedValue(status(true, true, false));
		await gateFirstRun();
		expect(setSession).not.toHaveBeenCalled();
		expect(goto).toHaveBeenCalledOnce();
	});

	it('does nothing when signed in, or when the check fails', async () => {
		get.mockResolvedValueOnce(status(true, false, false));
		await gateFirstRun();
		get.mockRejectedValueOnce(new Error('offline'));
		await gateFirstRun();
		expect(goto).not.toHaveBeenCalled();
		expect(toSetup).not.toHaveBeenCalled();
		expect(setSession).not.toHaveBeenCalled();
	});
});
