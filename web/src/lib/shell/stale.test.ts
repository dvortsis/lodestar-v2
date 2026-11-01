import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { STALE_THROTTLE_MS, hasUnsaved, watchStale } from './stale';
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

describe('the banner', () => {
	it('says a newer version is running, and nothing about changes when there are none', () => {
		render(StaleBanner);
		expect(screen.getByTestId('stale-banner').textContent).toContain(
			'A newer version of Lodestar is running. Reload to update.'
		);
		expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy();
		expect(screen.getByTestId('stale-banner').textContent).not.toContain('Save or discard');
		expect(screen.getByRole('status')).toBeTruthy();
	});

	it('asks to save or discard when a leave guard objects', () => {
		const guard = (e: Event) => e.preventDefault();
		window.addEventListener('beforeunload', guard);
		try {
			expect(hasUnsaved()).toBe(true);
			render(StaleBanner);
			expect(screen.getByTestId('stale-banner').textContent).toContain(
				'Save or discard your changes first.'
			);
		} finally {
			window.removeEventListener('beforeunload', guard);
		}
		expect(hasUnsaved()).toBe(false);
	});
});
