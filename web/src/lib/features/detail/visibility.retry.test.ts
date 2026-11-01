import { expect, it, vi } from 'vitest';

// The subscription chunk fails once, then loads: the next call subscribes.
let attempts = 0;
const subscribe = vi.fn();
vi.mock('./visibility-events', () => {
	attempts += 1;
	if (attempts === 1) throw new Error('chunk failed');
	return { subscribeDetailVisibility: subscribe };
});
const { watchDetailVisibility } = await import('./visibility');

it('retries the subscription on the next call after a failed chunk load', async () => {
	watchDetailVisibility();
	await vi.waitFor(() => expect(attempts).toBe(1), { timeout: 10_000 });
	await Promise.resolve();
	expect(subscribe).not.toHaveBeenCalled();
	vi.resetModules();
	watchDetailVisibility();
	await vi.waitFor(() => expect(subscribe).toHaveBeenCalledTimes(1), { timeout: 10_000 });
});
