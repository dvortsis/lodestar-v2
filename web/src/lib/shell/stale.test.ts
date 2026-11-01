import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { STALE_THROTTLE_MS, fetchBuild as realFetch, hasUnsaved, watchStale } from './stale';
import StaleBanner from './StaleBanner.svelte';

const show = () => document.dispatchEvent(new Event('visibilitychange'));
const flush = () => new Promise((r) => setTimeout(r));

let stop: (() => void) | undefined;
afterEach(() => stop?.());

function setup(server: string | null | Error) {
	let t = 1_000;
	const fetchBuild = vi.fn(async () => {
		if (server instanceof Error) throw server;
		return server;
	});
	const onStale = vi.fn();
	stop = watchStale(onStale, {
		fetchBuild: () => fetchBuild().catch(() => null),
		now: () => t,
		build: 'a'
	});
	return { fetchBuild, onStale, advance: (ms: number) => (t += ms) };
}

describe('watchStale', () => {
	it('same build: no banner callback', async () => {
		const s = setup('a');
		s.advance(STALE_THROTTLE_MS);
		show();
		await flush();
		expect(s.fetchBuild).toHaveBeenCalledTimes(1);
		expect(s.onStale).not.toHaveBeenCalled();
	});

	it('different build: calls onStale once', async () => {
		const s = setup('b');
		s.advance(STALE_THROTTLE_MS);
		show();
		await flush();
		show();
		await flush();
		expect(s.onStale).toHaveBeenCalledTimes(1);
	});

	it('a failed or empty check says nothing', async () => {
		for (const answer of [new Error('down'), null]) {
			const s = setup(answer);
			s.advance(STALE_THROTTLE_MS);
			show();
			await flush();
			expect(s.onStale).not.toHaveBeenCalled();
			stop?.();
		}
	});

	it('throttles to once per five minutes and never stacks', async () => {
		let release!: (v: string) => void;
		const fetchBuild = vi.fn(() => new Promise<string>((r) => (release = r)));
		let t = 0;
		const onStale = vi.fn();
		stop = watchStale(onStale, { fetchBuild, now: () => t, build: 'a' });
		show();
		expect(fetchBuild).not.toHaveBeenCalled(); // shown again within the first five minutes
		t = STALE_THROTTLE_MS;
		show();
		show(); // in flight: not stacked
		expect(fetchBuild).toHaveBeenCalledTimes(1);
		release('a');
		await flush();
		t += STALE_THROTTLE_MS - 1;
		show();
		expect(fetchBuild).toHaveBeenCalledTimes(1);
		t += 1;
		show();
		expect(fetchBuild).toHaveBeenCalledTimes(2);
		release('a');
	});
});

describe('fetchBuild', () => {
	const answer = (res: Partial<Response> | Error) =>
		vi.stubGlobal('fetch', async () => {
			if (res instanceof Error) throw res;
			return res;
		});
	afterEach(() => vi.unstubAllGlobals());

	it('reads the version', async () => {
		answer({ ok: true, json: async () => ({ version: '1.2-abc' }) });
		expect(await realFetch()).toBe('1.2-abc');
	});

	it.each([
		['a thrown fetch', new Error('down')],
		['a non-OK answer', { ok: false, json: async () => ({ version: 'x' }) }],
		['a non-JSON body', { ok: true, json: async () => Promise.reject(new Error('x')) }],
		['no version', { ok: true, json: async () => ({}) }],
		['an empty version', { ok: true, json: async () => ({ version: '' }) }],
		['a non-string version', { ok: true, json: async () => ({ version: 3 }) }],
		['a null body', { ok: true, json: async () => null }]
	])('says nothing for %s', async (_, res) => {
		answer(res as Partial<Response> | Error);
		expect(await realFetch()).toBeNull();
	});
});

describe('while hidden or after a rejected check', () => {
	it('does not ask while the tab is hidden', () => {
		const fetchBuild = vi.fn(async () => 'b');
		stop = watchStale(vi.fn(), { fetchBuild, now: () => STALE_THROTTLE_MS, build: 'a' });
		const spy = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
		show();
		spy.mockRestore();
		expect(fetchBuild).not.toHaveBeenCalled();
	});

	it('a rejected check does not stop later ones', async () => {
		let t = 0;
		const fetchBuild = vi.fn(async () => {
			throw new Error('x');
		});
		stop = watchStale(vi.fn(), { fetchBuild, now: () => t, build: 'a' });
		t = STALE_THROTTLE_MS;
		show();
		await flush();
		t += STALE_THROTTLE_MS;
		show();
		expect(fetchBuild).toHaveBeenCalledTimes(2);
	});
});

describe('the banner', () => {
	it('mounts the live region empty, with Reload, and fills it afterwards', async () => {
		render(StaleBanner);
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
		expect(screen.getByRole('status').textContent).not.toContain('A newer version');
		await screen.findByText(/A newer version of Lodestar is running/);
	});

	it('says a newer version is running, and nothing about changes when there are none', async () => {
		render(StaleBanner);
		await screen.findByText(/A newer version of Lodestar is running\. Reload to update\./);
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
		expect(screen.getByTestId('stale-banner').textContent).not.toContain('Save or discard');
		expect(screen.getByRole('status')).toBeTruthy();
	});

	it('asks to save or discard when a leave guard objects', async () => {
		const guard = (e: Event) => e.preventDefault();
		window.addEventListener('beforeunload', guard);
		try {
			expect(hasUnsaved()).toBe(true);
			render(StaleBanner);
			await screen.findByText(/Save or discard your changes first\./);
		} finally {
			window.removeEventListener('beforeunload', guard);
		}
		expect(hasUnsaved()).toBe(false);
	});
});
