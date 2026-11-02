import { describe, expect, it } from 'vitest';
import { anyActive, controls, isActive, processed, ratio, viewOf, type Job } from './jobs';

const job = (over: Partial<Job> = {}): Job => ({
	done: 0,
	eta_s: null,
	finished_at: null,
	id: 1,
	kind: 'export',
	rate: null,
	skipped: 0,
	started_at: 1_700_000_000,
	state: 'running',
	total: null,
	total_approx: false,
	...over
});

describe('viewOf', () => {
	it('shows a cancel the worker has not reached as cancelling', () => {
		expect(viewOf(job({ state: 'cancelled', finished_at: null }))).toBe('cancelling');
	});
	it('shows a finished cancel as cancelled', () => {
		expect(viewOf(job({ state: 'cancelled', finished_at: 5 }))).toBe('cancelled');
	});
	it('passes the other states through', () => {
		for (const state of ['running', 'paused', 'done', 'failed'] as const)
			expect(viewOf(job({ state, finished_at: state === 'running' ? null : 5 }))).toBe(state);
	});
});

describe('ratio and processed', () => {
	it('counts skipped items as read', () => {
		const j = job({ done: 30, skipped: 20, total: 100 });
		expect(processed(j)).toBe(50);
		expect(ratio(j)).toBe(0.5);
	});
	it('is null while the total is unknown', () => {
		expect(ratio(job({ done: 3 }))).toBeNull();
	});
	it('stays under 100% while running, even past an estimate that was too low', () => {
		expect(ratio(job({ done: 12, total: 10, total_approx: true }))).toBe(0.99);
		expect(ratio(job({ done: 999, total: 1000 }))).toBe(0.99);
	});
	it('follows an estimate that was too high up to the end', () => {
		expect(ratio(job({ done: 300, total: 1000, total_approx: true }))).toBe(0.3);
		expect(ratio(job({ done: 300, total: 1000, state: 'done', finished_at: 5 }))).toBe(1);
	});
	it('caps a finished, not done, job at 1', () => {
		expect(ratio(job({ done: 12, total: 10, state: 'failed', finished_at: 5 }))).toBe(1);
	});
	it('handles an empty total', () => {
		expect(ratio(job({ total: 0 }))).toBe(0);
		expect(ratio(job({ total: 0, state: 'done', finished_at: 9 }))).toBe(1);
	});
});

describe('controls', () => {
	it('offers pause and cancel while running', () => {
		expect(controls(job())).toEqual(['pause', 'cancel']);
	});
	it('offers resume and cancel while paused', () => {
		expect(controls(job({ state: 'paused' }))).toEqual(['resume', 'cancel']);
	});
	it('offers nothing once cancelling or finished', () => {
		expect(controls(job({ state: 'cancelled' }))).toEqual([]);
		expect(controls(job({ state: 'done', finished_at: 1 }))).toEqual([]);
		expect(controls(job({ state: 'failed', finished_at: 1 }))).toEqual([]);
	});
});

describe('activity', () => {
	it('is active until a finish time', () => {
		expect(isActive(job())).toBe(true);
		expect(isActive(job({ state: 'done', finished_at: 1 }))).toBe(false);
		expect(anyActive([job({ state: 'done', finished_at: 1 })])).toBe(false);
		expect(anyActive([job({ state: 'done', finished_at: 1 }), job({ id: 2 })])).toBe(true);
	});
});
