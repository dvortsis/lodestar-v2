// Per-worker breakdowns, Marginal gain, and the fleet pill's data and text (WK-17).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import StatusPill from '$lib/shell/StatusPill.svelte';
import WorkerBreakdown from './WorkerBreakdown.svelte';
import { watchFleet, FLEET_POLL_MS } from './fleet';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (actual) => ({
	...(await actual<typeof import('$lib/api/client')>()),
	api: { get }
}));

const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
const disc = [
	{ worker: 0, granted: 1000, marginal: 250, rediscovered: 10, dropped: 2 },
	{ worker: 3, granted: 0, marginal: 0, rediscovered: 0, dropped: 0 },
	{ worker: 7, granted: 400, marginal: 100, rediscovered: 4, dropped: 1 }
];
const fleet = { workers: 3, online: 2, draining: 1, paused: 1, offline: 2, stored_per_hour: 4200 };
const none = { torrents: 5, done: null, total: null };

beforeEach(() => {
	get.mockReset();
	initLocale('en');
});
afterEach(() => vi.useRealTimers());

describe('WorkerBreakdown', () => {
	it('renders nothing when by_worker is absent or empty', () => {
		const a = render(WorkerBreakdown, { stage: 'discover', byWorker: undefined });
		expect(a.container.textContent?.trim()).toBe('');
		a.unmount();
		const b = render(WorkerBreakdown, { stage: 'fetch', byWorker: [] });
		expect(b.container.textContent?.trim()).toBe('');
		expect(get).not.toHaveBeenCalled();
	});

	it('names for a non-admin: This server and Worker <id>', async () => {
		get.mockRejectedValue(new Error('403'));
		render(WorkerBreakdown, { stage: 'discover', byWorker: disc });
		await until(() => expect(get).toHaveBeenCalledWith('/workers', expect.anything()));
		const t = screen.getByTestId('insights-workers-table');
		expect(t.textContent).toContain('This server');
		expect(t.textContent).toContain('Worker 3');
		expect(t.textContent).toContain('Worker 7');
		expect(t.querySelector('caption')).toBeTruthy();
		expect(t.querySelectorAll('th[scope="col"]').length).toBe(5);
	});

	it('uses names from /workers for an admin, id for ones it lacks', async () => {
		get.mockResolvedValue({ allowed: true, workers: [{ id: 3, name: 'garage' }] });
		render(WorkerBreakdown, { stage: 'discover', byWorker: disc });
		await until(() =>
			expect(screen.getByTestId('insights-workers-table').textContent).toContain('garage')
		);
		const t = screen.getByTestId('insights-workers-table').textContent;
		expect(t).toContain('This server');
		expect(t).toContain('Worker 7');
	});

	it('shows marginal as count and share, with ≈ and the definition', async () => {
		get.mockRejectedValue(new Error('403'));
		render(WorkerBreakdown, { stage: 'discover', byWorker: disc });
		const t = screen.getByTestId('insights-marginal-table');
		expect(t.textContent).toContain('25%');
		expect(t.textContent).toContain('—');
		expect(t.textContent).toContain('Marginal gain ≈');
		expect(screen.getByTestId('insights-marginal').querySelector('h2')?.textContent).toContain('≈');
		const def = screen.getByTestId('insights-marginal-definition').textContent;
		expect(def).toContain('no other worker claimed');
		expect(def).toContain('under-counts');
	});

	it('Fetch has its table and the reason-filter note, no marginal view', async () => {
		get.mockRejectedValue(new Error('403'));
		render(WorkerBreakdown, {
			stage: 'fetch',
			byWorker: [{ worker: 0, committed: 9, lost: 1 }]
		});
		expect(screen.getByTestId('insights-workers').textContent).toContain('does not narrow');
		expect(screen.queryByTestId('insights-marginal')).toBeNull();
	});
});

describe('fleet pill', () => {
	it('adds workers and rate, with paused and offline only in the description', async () => {
		get.mockResolvedValue({ fleet });
		render(StatusPill, { state: { kind: 'crawling', ...none } });
		await until(() =>
			expect(screen.getByTestId('status-pill-fleet').textContent).toBe('· 3 workers · 4.2K/h')
		);
		const link = screen.getByTestId('status-pill');
		expect(screen.getByTestId('status-pill-fleet').textContent).not.toMatch(/paused|offline/);
		expect(screen.getByTestId('status-pill-fleet-help').textContent).toBe(
			'3 working, 1 paused, 2 offline.'
		);
		expect(link.textContent).toContain('Crawling');
	});

	it('adds nothing on fleet: null or a failed request', async () => {
		get.mockResolvedValue({ fleet: null });
		const a = render(StatusPill, { state: { kind: 'crawling', ...none } });
		await until(() => expect(get).toHaveBeenCalled());
		expect(screen.queryByTestId('status-pill-fleet')).toBeNull();
		a.unmount();
		get.mockRejectedValue(new Error('x'));
		render(StatusPill, { state: { kind: 'crawling', ...none } });
		await until(() => expect(get).toHaveBeenCalledTimes(2));
		expect(screen.queryByTestId('status-pill-fleet')).toBeNull();
	});
});

describe('watchFleet', () => {
	it('polls once a minute, stops while hidden, and on unmount', async () => {
		vi.useFakeTimers();
		get.mockResolvedValue({ fleet });
		const set = vi.fn();
		const stop = watchFleet(set);
		await vi.advanceTimersByTimeAsync(0);
		expect(get).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(FLEET_POLL_MS - 1);
		expect(get).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(1);
		expect(get).toHaveBeenCalledTimes(2);

		Object.defineProperty(document, 'hidden', { configurable: true, value: true });
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.advanceTimersByTimeAsync(FLEET_POLL_MS * 3);
		expect(get).toHaveBeenCalledTimes(2);

		Object.defineProperty(document, 'hidden', { configurable: true, value: false });
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.advanceTimersByTimeAsync(0);
		expect(get).toHaveBeenCalledTimes(3);

		stop();
		await vi.advanceTimersByTimeAsync(FLEET_POLL_MS * 3);
		expect(get).toHaveBeenCalledTimes(3);
		expect(set).toHaveBeenCalledWith(fleet);
	});

	it('aborts an in-flight request on stop', async () => {
		let signal: AbortSignal | undefined;
		get.mockImplementation((_p: string, o: { signal: AbortSignal }) => {
			signal = o.signal;
			return new Promise(() => {});
		});
		const stop = watchFleet(() => {});
		expect(signal?.aborted).toBe(false);
		stop();
		expect(signal?.aborted).toBe(true);
	});
});
