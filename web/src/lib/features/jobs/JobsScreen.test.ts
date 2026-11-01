import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, configure, fireEvent, render, screen } from '@testing-library/svelte';
import { ApiError } from '$lib/api/client';
import type { Job } from './jobs';

const get = vi.hoisted(() => vi.fn());
const post = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (orig) => ({
	...(await orig<typeof import('$lib/api/client')>()),
	api: { get, post }
}));

import JobsScreen from './JobsScreen.svelte';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
const wait = (check: () => void) => vi.waitFor(check, { timeout: 10_000 });

const job = (over: Partial<Job> = {}): Job => ({
	done: 40,
	eta_s: 90,
	finished_at: null,
	id: 3,
	kind: 'export',
	rate: 12.5,
	skipped: 10,
	started_at: 1_700_000_000,
	state: 'running',
	total: 100,
	total_approx: false,
	...over
});

beforeEach(() => {
	get.mockReset();
	post.mockReset();
});
afterEach(cleanup);

describe('JobsScreen', () => {
	it('shows progress, rate and ETA from the server', async () => {
		get.mockResolvedValue({ jobs: [job()] });
		render(JobsScreen);
		await wait(() =>
			expect(screen.getByTestId('job-3-progress').textContent).toContain('50 of 100')
		);
		expect(screen.getByTestId('job-3-progress').textContent).toContain('50%');
		expect(screen.getByTestId('job-3-rate').textContent).toBe('13 items/s');
		expect(screen.getByTestId('job-3-eta').textContent).toBe('1.5 min');
		expect(screen.getByRole('progressbar', { name: 'Export #3 progress' })).toBeTruthy();
	});

	it('shows an empty state', async () => {
		get.mockResolvedValue({ jobs: [] });
		render(JobsScreen);
		await wait(() => expect(screen.getByTestId('jobs-empty')).toBeTruthy());
	});

	it('shows a failed load and retries', async () => {
		get.mockRejectedValueOnce(new ApiError(500, 'internal', 'boom'));
		render(JobsScreen);
		await wait(() => expect(screen.getByTestId('jobs-retry')).toBeTruthy());
		get.mockResolvedValue({ jobs: [job()] });
		await fireEvent.click(screen.getByTestId('jobs-retry'));
		await wait(() => expect(screen.getByTestId('job-3')).toBeTruthy());
	});

	it('changes state only to what the server answers', async () => {
		get.mockResolvedValue({ jobs: [job()] });
		let answer!: (j: Job) => void;
		post.mockReturnValue(new Promise<Job>((r) => (answer = r)));
		render(JobsScreen);
		await wait(() => expect(screen.getByTestId('job-3-pause')).toBeTruthy());
		await fireEvent.click(screen.getByTestId('job-3-pause'));
		expect(post).toHaveBeenCalledWith('/jobs/{id}/pause', { path: { id: 3 }, body: {} });
		// Asked, not yet answered: still running.
		expect(screen.getByTestId('job-3-state').textContent).toBe('Running');
		answer(job({ state: 'paused', rate: 12.5, eta_s: null }));
		await wait(() => expect(screen.getByTestId('job-3-state').textContent).toBe('Paused'));
		expect(screen.getByTestId('job-3-resume')).toBeTruthy();
		expect(screen.getByTestId('jobs-announce').textContent).toBe('Export #3: Paused');
	});

	it('shows a cancel the worker has not reached as Cancelling, without controls', async () => {
		get.mockResolvedValue({ jobs: [job({ state: 'cancelled' })] });
		render(JobsScreen);
		await wait(() => expect(screen.getByTestId('job-3-state').textContent).toBe('Cancelling'));
		expect(screen.queryByTestId('job-3-cancel')).toBeNull();
	});

	it('shows why a control was refused and what the server has now', async () => {
		get.mockResolvedValueOnce({ jobs: [job()] });
		post.mockRejectedValue(new ApiError(409, 'job_finished', 'That job has already finished.'));
		render(JobsScreen);
		await wait(() => expect(screen.getByTestId('job-3-cancel')).toBeTruthy());
		get.mockResolvedValue({ jobs: [job({ state: 'done', finished_at: 1_700_000_100 })] });
		await fireEvent.click(screen.getByTestId('job-3-cancel'));
		await wait(() => expect(screen.getByTestId('job-3-state').textContent).toBe('Done'));
		expect(screen.getByTestId('job-3-refused').textContent).toContain('already finished');
	});

	it('shows a failed job’s error and a truncated export', async () => {
		get.mockResolvedValue({
			jobs: [
				job({
					state: 'failed',
					finished_at: 1_700_000_100,
					error: { code: 'x', message: 'It broke.' },
					export: { cap: 1000, format: 'csv', truncated: true }
				})
			]
		});
		render(JobsScreen);
		await wait(() => expect(screen.getByTestId('job-3-error').textContent).toBe('It broke.'));
		expect(screen.getByTestId('job-3-truncated')).toBeTruthy();
	});

	it('marks old rows when a refresh fails', async () => {
		vi.useFakeTimers({ shouldAdvanceTime: true });
		try {
			get.mockResolvedValueOnce({ jobs: [job()] });
			render(JobsScreen);
			await wait(() => expect(screen.getByTestId('job-3')).toBeTruthy());
			get.mockRejectedValue(new ApiError(0, 'network', 'offline'));
			await vi.advanceTimersByTimeAsync(2100);
			await wait(() => expect(screen.getByTestId('jobs-stale')).toBeTruthy());
			expect(screen.getByTestId('job-3')).toBeTruthy();
		} finally {
			vi.useRealTimers();
		}
	});
});
