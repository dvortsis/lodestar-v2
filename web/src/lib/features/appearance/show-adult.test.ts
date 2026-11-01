import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';

const patch = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { patch }
}));

import { SHOW_ADULT_ENV as flowsEnv, setShowAdult } from '../filters/flows/adult.svelte';
import { SHOW_ADULT_ENV, showAdultOff } from './show-adult';

const never = { ask: () => Promise.resolve(false), open: false, answer() {} };

beforeEach(() => {
	patch.mockReset();
	patch.mockResolvedValue({ revision: 3 });
});

describe('show-adult', () => {
	it("names the flows' variable", () => expect(SHOW_ADULT_ENV).toBe(flowsEnv));

	it("sends what the flows' turn-off sends", async () => {
		expect(await showAdultOff()).toEqual(await setShowAdult(false, never));
		expect(patch.mock.calls[0]).toEqual(patch.mock.calls[1]);
		expect(patch.mock.calls[0]![1].body).toEqual({ show_adult: false });
	});

	it("fails as the flows' turn-off fails", async () => {
		const outcome = (r: Awaited<ReturnType<typeof showAdultOff>>) =>
			r.ok ? 'ok' : `${r.error?.status}:${r.error?.code}`;
		patch.mockRejectedValue(new ApiError(500, 'internal', 'x'));
		expect(outcome(await showAdultOff())).toBe('500:internal');
		expect(outcome(await setShowAdult(false, never))).toBe('500:internal');
		patch.mockRejectedValue(new TypeError('offline'));
		expect(outcome(await showAdultOff())).toBe('0:network');
		expect(outcome(await setShowAdult(false, never))).toBe('0:network');
	});
});
