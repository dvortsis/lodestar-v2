import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { backoff, GIVE_UP_MS, pollUntil, restartedHere, upAt } from './restart';

const system = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('$lib/api/client', async (actual) => ({
	...(await actual<typeof import('$lib/api/client')>()),
	api: { get: system.get }
}));

afterEach(() => vi.clearAllMocks());

/** A clock the fake sleep advances. */
function clock() {
	let t = 0;
	const waits: number[] = [];
	return {
		waits,
		now: () => t,
		sleep: async (ms: number) => {
			waits.push(ms);
			t += ms;
		}
	};
}

describe('backoff', () => {
	it('doubles from 1 s to 8 s and stays there', () => {
		expect([0, 1, 2, 3, 4, 9].map(backoff)).toEqual([1000, 2000, 4000, 8000, 8000, 8000]);
	});
});

describe('pollUntil', () => {
	it('answers up as soon as the check does, waiting 1 s, 2 s, 4 s between tries', async () => {
		const c = clock();
		let tries = 0;
		const out = await pollUntil(async () => ++tries === 4, c);
		expect(out).toBe('up');
		expect(c.waits).toEqual([1000, 2000, 4000]);
	});

	it('counts a failing check as not yet', async () => {
		const c = clock();
		let tries = 0;
		const out = await pollUntil(async () => {
			if (++tries < 3) throw new TypeError('Failed to fetch');
			return true;
		}, c);
		expect(out).toBe('up');
	});

	it('gives up after two minutes', async () => {
		const c = clock();
		const out = await pollUntil(async () => false, c);
		expect(out).toBe('timeout');
		expect(c.waits.reduce((a, b) => a + b, 0)).toBeGreaterThanOrEqual(GIVE_UP_MS);
		expect(c.waits.at(-1)).toBe(8000);
	});

	it('stops when aborted', async () => {
		const ctl = new AbortController();
		const out = await pollUntil(
			async () => {
				ctl.abort();
				return false;
			},
			{ ...clock(), signal: ctl.signal }
		);
		expect(out).toBe('aborted');
	});
});

describe('restartedHere', () => {
	const ok = { ok: true } as Response;
	const bad = { ok: false } as Response;

	it('is not restarted while the old process still has the same start time', async () => {
		system.get.mockResolvedValue({ started_at: 100 });
		expect(
			await restartedHere(
				100,
				vi.fn(async () => ok)
			)()
		).toBe(false);
	});

	it('is restarted once /healthz answers and the start time moved', async () => {
		system.get.mockResolvedValue({ started_at: 160 });
		expect(
			await restartedHere(
				100,
				vi.fn(async () => ok)
			)()
		).toBe(true);
	});

	it('waits while /healthz answers with an error', async () => {
		expect(
			await restartedHere(
				100,
				vi.fn(async () => bad)
			)()
		).toBe(false);
		expect(system.get).not.toHaveBeenCalled();
	});

	it('waits while /system fails with a network or server error, and counts a 401 as up', async () => {
		const fetcher = vi.fn(async () => ok);
		system.get.mockRejectedValueOnce(new ApiError(0, 'network', 'offline'));
		expect(await restartedHere(100, fetcher)()).toBe(false);
		system.get.mockRejectedValueOnce(new ApiError(503, 'unavailable', 'starting'));
		expect(await restartedHere(100, fetcher)()).toBe(false);
		system.get.mockRejectedValueOnce(new ApiError(401, 'login_required', 'sign in'));
		expect(await restartedHere(100, fetcher)()).toBe(true);
	});
});

describe('upAt', () => {
	it('asks the new address’s /healthz with no-cors, and any answer is up', async () => {
		const fetcher = vi.fn(
			async () => ({ type: 'opaque', ok: false, status: 0 }) as unknown as Response
		);
		expect(await upAt('http://192.168.1.5:9090', fetcher as never)()).toBe(true);
		expect(fetcher).toHaveBeenCalledWith(new URL('http://192.168.1.5:9090/healthz'), {
			mode: 'no-cors',
			cache: 'no-store'
		});
	});

	it('fails (so polling goes on) when nothing is listening yet', async () => {
		const fetcher = vi.fn(async () => Promise.reject(new TypeError('Failed to fetch')));
		await expect(upAt('http://192.168.1.5:9090', fetcher as never)()).rejects.toThrow();
	});

	it('never asks an address that is not http(s)', async () => {
		const fetcher = vi.fn();
		expect(await upAt('javascript:alert(1)', fetcher as never)()).toBe(false);
		expect(fetcher).not.toHaveBeenCalled();
	});
});
