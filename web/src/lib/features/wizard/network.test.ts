import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createNetworkCheck } from './network.svelte';
import { network, wizardServer } from './testing/server';

let server: ReturnType<typeof wizardServer>;
beforeEach(() => {
	vi.useFakeTimers();
	server = wizardServer();
	vi.stubGlobal('fetch', server.fetch);
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

const settle = () => vi.advanceTimersByTimeAsync(0);

describe('network check', () => {
	it('reads once when the answer is known', async () => {
		const net = createNetworkCheck({ intervalMs: 1000 });
		net.start();
		await settle();
		expect(net.check?.reachable).toBe(true);
		await vi.advanceTimersByTimeAsync(10_000);
		expect(server.netCalls).toEqual(['GET']);
		net.destroy();
	});

	it('reads again while unknown, until it knows', async () => {
		server.setNetwork(network({ reachable: null, reason: 'measuring', measured_s: 30 }));
		const net = createNetworkCheck({ intervalMs: 1000 });
		net.start();
		await settle();
		await vi.advanceTimersByTimeAsync(2000);
		expect(server.netCalls.length).toBeGreaterThanOrEqual(3);
		server.setNetwork(network());
		await vi.advanceTimersByTimeAsync(1000);
		const reads = server.netCalls.length;
		expect(net.check?.reachable).toBe(true);
		await vi.advanceTimersByTimeAsync(5000);
		expect(server.netCalls.length).toBe(reads);
		net.destroy();
	});

	it('never stacks: the next read waits for the last answer', async () => {
		server.setNetwork(network({ reachable: null, reason: 'measuring' }));
		const inner = server.fetch;
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		let started = 0;
		vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
			started++;
			await gate;
			return inner(url, init);
		});
		const net = createNetworkCheck({ intervalMs: 100 });
		net.start();
		await vi.advanceTimersByTimeAsync(5000);
		expect(started).toBe(1); // no second request while the first has no answer
		release();
		await vi.advanceTimersByTimeAsync(100);
		expect(started).toBe(2);
		net.destroy();
	});

	it('stops at the deadline and says so', async () => {
		server.setNetwork(network({ reachable: null, reason: 'measuring' }));
		const net = createNetworkCheck({ intervalMs: 1000, deadlineMs: 5000 });
		net.start();
		await vi.advanceTimersByTimeAsync(5000);
		await settle();
		expect(net.stopped).toBe(true);
		const reads = server.netCalls.length;
		await vi.advanceTimersByTimeAsync(20_000);
		expect(server.netCalls.length).toBe(reads);
		net.destroy();
	});

	it('stops reading, and ignores an answer in flight, once destroyed', async () => {
		server.setNetwork(network({ reachable: null, reason: 'measuring' }));
		const net = createNetworkCheck({ intervalMs: 1000 });
		net.start();
		await settle();
		net.destroy();
		const reads = server.netCalls.length;
		await vi.advanceTimersByTimeAsync(10_000);
		expect(server.netCalls.length).toBe(reads);
	});

	it('Retest posts, restarts the measurement and reads again', async () => {
		const net = createNetworkCheck({ intervalMs: 1000 });
		net.start();
		await settle();
		server.setNetwork((method) =>
			server.json(
				200,
				method === 'POST'
					? network({ reachable: null, reason: 'bootstrapping', measured_s: 0 })
					: network()
			)
		);
		net.retestNow();
		await settle();
		expect(server.netCalls.at(-1)).toBe('POST');
		expect(net.check?.reachable).toBeNull();
		expect(net.retest).toBe('idle');
		await vi.advanceTimersByTimeAsync(1000);
		expect(server.netCalls.at(-1)).toBe('GET');
		net.destroy();
	});

	it('names the 429 plainly and keeps the last answer', async () => {
		const net = createNetworkCheck({ intervalMs: 1000 });
		net.start();
		await settle();
		server.setNetwork(() =>
			server.json(
				429,
				{ error: { code: 'rate_limited', message: 'slow down' } },
				{ 'Retry-After': '17' }
			)
		);
		net.retestNow();
		await settle();
		expect(net.retest).toBe('limited');
		expect(net.retryAfter).toBe(17);
		expect(net.check?.reachable).toBe(true);
		net.destroy();
	});

	it('a failed read is shown but not fatal', async () => {
		server.setNetwork(() => server.json(500, { error: { code: 'internal', message: 'x' } }));
		const net = createNetworkCheck({ intervalMs: 1000 });
		net.start();
		await settle();
		expect(net.failed).toBe(true);
		expect(net.check).toBeNull();
		net.destroy();
	});
});
