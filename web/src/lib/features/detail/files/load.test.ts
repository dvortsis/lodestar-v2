import { describe, expect, it, vi } from 'vitest';
import { LANES, PAGE, WORKER_THRESHOLD, loadRest } from './load';

const pageOf = (offset: number, total: number) =>
	Array.from({ length: Math.min(PAGE, total - offset) }, (_, i) => offset + i);
const run = (over: Partial<Parameters<typeof loadRest<number>>[0]> = {}) => {
	const got: number[] = [];
	const options = {
		loaded: 500,
		total: 3200,
		fetchPage: async (offset: number) => pageOf(offset, 3200),
		onPage: (files: number[]) => got.push(...files),
		signal: new AbortController().signal,
		...over
	};
	return { got, done: loadRest<number>(options) };
};

describe('loadRest', () => {
	it('pages from the held count to the total, 1000 at a time, in order', async () => {
		const offsets: number[] = [];
		const { got, done } = run({
			fetchPage: async (offset, limit) => {
				offsets.push(offset);
				expect(limit).toBe(1000);
				return pageOf(offset, 3200);
			}
		});
		await done;
		expect(offsets).toEqual([500, 1500, 2500]);
		expect(got).toEqual(Array.from({ length: 2700 }, (_, i) => 500 + i));
	});

	it('keeps at most two requests in flight and delivers in order when they finish out of order', async () => {
		let inflight = 0;
		let peak = 0;
		const pending = new Map<number, () => void>();
		const { got, done } = run({
			total: 4500,
			fetchPage: (offset) =>
				new Promise((resolve) => {
					inflight++;
					peak = Math.max(peak, inflight);
					pending.set(offset, () => {
						inflight--;
						resolve(pageOf(offset, 4500));
					});
				})
		});
		await vi.waitFor(() => expect(pending.size).toBe(LANES));
		pending.get(1500)!(); // the later page finishes first
		await Promise.resolve();
		expect(got).toEqual([]);
		pending.get(500)!();
		await vi.waitFor(() => expect(got).toHaveLength(2000));
		for (const offset of [2500, 3500]) {
			await vi.waitFor(() => expect(pending.has(offset)).toBe(true));
			pending.get(offset)!();
		}
		await done;
		expect(peak).toBeLessThanOrEqual(LANES);
		expect(got).toHaveLength(4000);
		expect(got).toEqual([...got].sort((a, b) => a - b));
	});

	it('keeps what loaded before a failure and rejects', async () => {
		const { got, done } = run({
			fetchPage: async (offset) => {
				if (offset >= 1500) throw new Error('down');
				return pageOf(offset, 3200);
			}
		});
		await expect(done).rejects.toThrow('down');
		expect(got).toHaveLength(1000);
	});

	it('stops when aborted and delivers nothing more', async () => {
		const ctl = new AbortController();
		const { got, done } = run({
			signal: ctl.signal,
			fetchPage: async (offset) => {
				ctl.abort();
				return pageOf(offset, 3200);
			}
		});
		await done;
		expect(got).toEqual([]);
	});

	it('does nothing when everything is already held', async () => {
		const fetchPage = vi.fn();
		await run({ loaded: 3200, fetchPage }).done;
		expect(fetchPage).not.toHaveBeenCalled();
	});

	it('uses the worker threshold of 2,000 files', () => {
		expect(WORKER_THRESHOLD).toBe(2000);
	});
});
