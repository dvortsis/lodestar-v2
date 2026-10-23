import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { createAuthForm } from './form.svelte';

beforeEach(() => vi.useFakeTimers({ now: 0 }));
afterEach(() => vi.useRealTimers());

const rateLimited = (s?: number) => new ApiError(429, 'rate_limited', '', { retryAfter: s });

describe('createAuthForm', () => {
	it('submits once: a second submit while sending is ignored', async () => {
		const form = createAuthForm();
		let release!: () => void;
		const request = vi.fn(() => new Promise<void>((r) => (release = r)));
		const first = form.submit(request);
		expect(form.busy).toBe(true);
		expect(await form.submit(request)).toBe(false);
		expect(request).toHaveBeenCalledTimes(1);
		release();
		expect(await first).toBe(true);
		expect(form.state.name).toBe('success');
	});

	it('turns a rejection into an error state', async () => {
		const form = createAuthForm();
		const ok = await form.submit(() => Promise.reject(new ApiError(401, 'bad_password', '')));
		expect(ok).toBe(false);
		expect(form.state).toEqual({
			name: 'error',
			failure: { kind: 'bad_password', field: 'password' }
		});
		form.edit();
		expect(form.state.name).toBe('idle');
	});

	it('counts down a 429 and unlocks on its own', async () => {
		const form = createAuthForm();
		await form.submit(() => Promise.reject(rateLimited(3)));
		expect(form.state.name).toBe('locked');
		expect(form.secondsLeft).toBe(3);
		const request = vi.fn(() => Promise.resolve());
		expect(await form.submit(request)).toBe(false);
		expect(request).not.toHaveBeenCalled();
		vi.advanceTimersByTime(1000);
		expect(form.secondsLeft).toBe(2);
		vi.advanceTimersByTime(1000);
		expect(form.secondsLeft).toBe(1);
		vi.advanceTimersByTime(1000);
		expect(form.state.name).toBe('idle');
		expect(form.secondsLeft).toBe(0);
		expect(vi.getTimerCount()).toBe(0);
		expect(await form.submit(request)).toBe(true);
	});

	it('stops its timer when destroyed', async () => {
		const form = createAuthForm();
		await form.submit(() => Promise.reject(rateLimited(60)));
		expect(vi.getTimerCount()).toBe(1);
		form.destroy();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('records client-side validation without sending', () => {
		const form = createAuthForm();
		form.invalid({ kind: 'invalid', field: 'confirm', reason: 'mismatch' });
		expect(form.state).toEqual({
			name: 'error',
			failure: { kind: 'invalid', field: 'confirm', reason: 'mismatch' }
		});
	});
});
