import { describe, expect, it, vi } from 'vitest';
import { backoffMs, serverAnswers, watchServer } from './reach';

describe('backoffMs', () => {
	it('doubles from 1 s and stops at 30 s', () => {
		expect([0, 1, 2, 3, 4, 5, 6, 20].map(backoffMs)).toEqual([
			1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000
		]);
	});
});

describe('serverAnswers', () => {
	it('is true for any HTTP answer, including a 500', async () => {
		const f = vi.fn(async () => new Response('boom', { status: 500 }));
		expect(await serverAnswers(f as unknown as typeof fetch)).toBe(true);
		expect(f).toHaveBeenCalledWith('/healthz', expect.objectContaining({ cache: 'no-store' }));
	});

	it('is false when the request itself fails', async () => {
		const f = vi.fn(async () => {
			throw new TypeError('Failed to fetch');
		});
		expect(await serverAnswers(f as unknown as typeof fetch)).toBe(false);
	});
});

/** A hand-driven clock: timers run when the test says so. */
function harness(answers: boolean[]) {
	let pending: { fn: () => void; ms: number } | undefined;
	const states: unknown[] = [];
	const probe = vi.fn(async () => answers.shift() ?? true);
	const onUp = vi.fn();
	const watch = watchServer({
		probe,
		onUp,
		onState: (s) => states.push(s),
		setTimer: (fn, ms) => (pending = { fn, ms }),
		clearTimer: () => (pending = undefined)
	});
	const fire = async () => {
		const t = pending;
		pending = undefined;
		t?.fn();
		await vi.waitFor(() => expect(pending !== undefined || onUp.mock.calls.length > 0).toBe(true));
	};
	return { watch, states, probe, onUp, fire, delay: () => pending?.ms };
}

describe('watchServer', () => {
	it('waits before the first probe, backs off while down, then reports up once', async () => {
		const h = harness([false, false, true]);
		expect(h.delay()).toBe(1_000);
		expect(h.probe).not.toHaveBeenCalled();
		await h.fire();
		expect(h.delay()).toBe(2_000);
		await h.fire();
		expect(h.delay()).toBe(4_000);
		await h.fire();
		expect(h.onUp).toHaveBeenCalledOnce();
		expect(h.delay()).toBeUndefined();
	});

	it('Try again probes at once and replaces the timer', async () => {
		const h = harness([true]);
		h.watch.retryNow();
		await vi.waitFor(() => expect(h.onUp).toHaveBeenCalledOnce());
		expect(h.probe).toHaveBeenCalledOnce();
		expect(h.delay()).toBeUndefined();
	});

	it('stop cancels the timer and ignores a probe still out', async () => {
		const h = harness([true]);
		h.watch.stop();
		expect(h.delay()).toBeUndefined();
		h.watch.retryNow();
		await Promise.resolve();
		expect(h.onUp).not.toHaveBeenCalled();
	});
});
