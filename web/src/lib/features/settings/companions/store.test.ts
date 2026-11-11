// The restart watch and the size text (CMP-9a): after a restart the row is looked at every 3 s until
// it is healthy, at most 20 times; a second restart replaces the watch; leaving the screen leaves no
// timer behind. Time is faked, so nothing here sleeps.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CompanionsStore } from './store.svelte';
import { formatBytes } from '$lib/format';
import { bytesText } from './logic';

const reply = (code: number, b: unknown) =>
	new Response(JSON.stringify(b), {
		status: code,
		headers: { 'Content-Type': 'application/json' }
	});
const row = (health: string) => ({
	available: null,
	credentials: [],
	health,
	id: 'qbittorrent',
	installed: '5.0.0',
	licence: 'GPL-2.0-or-later',
	name: 'qBittorrent',
	opt_in: false,
	refusal: null,
	templates: []
});

let health: string;
let gets: number;
let hold: Promise<void> | null;
beforeEach(() => {
	vi.useFakeTimers();
	health = 'unhealthy';
	gets = 0;
	hold = null;
	vi.stubGlobal(
		'fetch',
		vi.fn(async (url: string, init: RequestInit = {}) => {
			const path = new URL(url, 'http://x').pathname.replace('/api/v1', '');
			if ((init.method ?? 'GET') === 'POST') return reply(202, { id: 'qbittorrent' });
			if (path === '/companions/qbittorrent') {
				gets++;
				const wait = hold;
				hold = null;
				if (wait) await wait;
				return reply(200, row(health));
			}
			return reply(200, { companions: [row(health)] });
		})
	);
});
afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

const started = async () => {
	const store = new CompanionsStore();
	store.start();
	await vi.advanceTimersByTimeAsync(0);
	return store;
};

describe('the restart watch', () => {
	it('looks every 3 s and stops once the companion is healthy', async () => {
		const store = await started();
		await store.restart('qbittorrent');
		const first = gets; // the refresh straight after the 202
		await vi.advanceTimersByTimeAsync(3000);
		expect(gets).toBe(first + 1);
		health = 'healthy';
		await vi.advanceTimersByTimeAsync(3000);
		expect(gets).toBe(first + 2);
		await vi.advanceTimersByTimeAsync(60_000);
		expect(gets).toBe(first + 2);
		expect(vi.getTimerCount()).toBe(0);
		store.stop();
	});
	it('gives up after 20 looks', async () => {
		const store = await started();
		await store.restart('qbittorrent');
		const first = gets;
		await vi.advanceTimersByTimeAsync(3000 * 40);
		expect(gets).toBe(first + 20);
		expect(vi.getTimerCount()).toBe(0);
		store.stop();
	});
	it('leaves no timer behind when the screen goes', async () => {
		const store = await started();
		await store.restart('qbittorrent');
		expect(vi.getTimerCount()).toBeGreaterThan(0);
		store.stop();
		expect(vi.getTimerCount()).toBe(0);
		const seen = gets;
		await vi.advanceTimersByTimeAsync(30_000);
		expect(gets).toBe(seen);
	});
	it('a second restart replaces the watch instead of stacking another', async () => {
		const store = await started();
		await store.restart('qbittorrent');
		await store.restart('qbittorrent');
		expect(vi.getTimerCount()).toBe(1);
		store.stop();
	});
});

describe('a look still running from an old watch', () => {
	it('does not re-arm over a newer, full-length watch', async () => {
		const store = await started();
		await store.restart('qbittorrent');
		let release: () => void = () => {};
		hold = new Promise<void>((r) => (release = r));
		await vi.advanceTimersByTimeAsync(3000); // the old watch looks, and waits
		await store.restart('qbittorrent'); // a newer watch, 20 looks
		const before = gets;
		release();
		await vi.advanceTimersByTimeAsync(3000 * 40);
		expect(gets - before).toBe(20);
		store.stop();
	});
});

describe('bytesText', () => {
	const t = (n: number) => bytesText(n, 'en');
	it('matches the shared format at the unit boundaries', () => {
		expect(t(0)).toBe('0 B');
		expect(t(1023)).toBe('1,023 B');
		expect(t(1024)).toBe('1.00 KiB');
		expect(t(1_048_575)).toBe('1.00 MiB');
		expect(t(2 ** 50)).toBe('1.00 PiB');
		expect(t(12_582_912)).toBe('12.0 MiB');
		expect(t(400 * 1024)).toBe('400 KiB');
	});
	it('is the shared formatter, value for value', () => {
		for (const n of [
			0,
			1,
			1023,
			1024,
			1536,
			1_048_575,
			1_048_576,
			12_582_912,
			2 ** 30 - 1,
			2 ** 40,
			2 ** 50,
			2 ** 52
		])
			expect(t(n)).toBe(formatBytes(n, 'en'));
	});
	it('prints a dash for sizes that are not sizes', () => {
		expect(t(Number.NaN)).toBe('—');
		expect(t(-1)).toBe('—');
		expect(t(Number.POSITIVE_INFINITY)).toBe('—');
	});
});
