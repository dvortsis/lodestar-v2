import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { crawlerFixture, overviewFixture } from './fixtures';

const get = vi.fn();
const patch = vi.fn();
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get, patch, post: vi.fn() }
}));
const { createInsights } = await import('./state.svelte');

let level = 'balanced';
beforeEach(() => {
	vi.useFakeTimers();
	level = 'balanced';
	get.mockImplementation((path: string) =>
		Promise.resolve(
			path === '/crawler' ? crawlerFixture({ intensity: level as 'low' }) : overviewFixture()
		)
	);
	patch.mockImplementation((_p: string, o: { body: { intensity: string } }) => {
		level = o.body.intensity;
		return Promise.resolve({});
	});
});
afterEach(() => {
	vi.useRealTimers();
	vi.clearAllMocks();
});

async function ready() {
	const s = createInsights();
	const off = s.start();
	await vi.advanceTimersByTimeAsync(0);
	return { s, off };
}

describe('Undo for the intensity click (U26)', () => {
	it('restores the previous level exactly, then offers nothing', async () => {
		const { s, off } = await ready();
		await s.setIntensity('aggressive');
		expect(s.undo).toEqual({ level: 'aggressive' });
		await s.undoIntensity();
		expect(level).toBe('balanced');
		expect(s.undo).toBeUndefined();
		off();
	});

	it('two changes within 10 s undo one step at a time', async () => {
		const { s, off } = await ready();
		await s.setIntensity('aggressive');
		await vi.advanceTimersByTimeAsync(4000);
		await s.setIntensity('low');
		expect(s.undo?.level).toBe('low');
		await s.undoIntensity();
		expect(level).toBe('aggressive');
		expect(s.undo).toEqual({ level: 'aggressive' });
		await s.undoIntensity();
		expect(level).toBe('balanced');
		expect(s.undo).toBeUndefined();
		off();
	});

	it('goes away 10 s after the last change, and not before', async () => {
		const { s, off } = await ready();
		await s.setIntensity('low');
		await vi.advanceTimersByTimeAsync(9_900);
		expect(s.undo).toBeDefined();
		await vi.advanceTimersByTimeAsync(200);
		expect(s.undo).toBeUndefined();
		off();
	});

	it('a failed change offers no Undo', async () => {
		const { s, off } = await ready();
		patch.mockRejectedValueOnce(new Error('no'));
		await s.setIntensity('low');
		expect(s.undo).toBeUndefined();
		off();
	});

	it('names the level that was set and the level put back', async () => {
		const { s, off } = await ready();
		await s.setIntensity('aggressive');
		expect(s.undo).toEqual({ level: 'aggressive' });
		await s.setIntensity('low');
		await s.undoIntensity();
		expect(s.undo).toEqual({ level: 'aggressive' });
		off();
	});

	it('runs one intensity action at a time: a click during an Undo is ignored', async () => {
		const { s, off } = await ready();
		await s.setIntensity('aggressive');
		const undoing = s.undoIntensity();
		await s.setIntensity('low');
		await undoing;
		expect(patch).toHaveBeenCalledTimes(2);
		expect(level).toBe('balanced');
		expect(s.undo).toBeUndefined();
		off();
	});

	it('a failed Undo keeps the step', async () => {
		const { s, off } = await ready();
		await s.setIntensity('aggressive');
		patch.mockRejectedValueOnce(new Error('no'));
		await s.undoIntensity();
		expect(s.undo).toEqual({ level: 'aggressive' });
		off();
	});

	it('expiry while Undo runs announces nothing, and focus on the line holds the 10 s', async () => {
		const { s, off } = await ready();
		await s.setIntensity('low');
		s.holdUndo(true);
		await vi.advanceTimersByTimeAsync(30_000);
		expect(s.undo).toBeDefined();
		s.holdUndo(false);
		await vi.advanceTimersByTimeAsync(10_100);
		expect(s.undo).toBeUndefined();
		off();
	});

	it('nothing is armed after the page is gone', async () => {
		const { s, off } = await ready();
		let release = () => {};
		patch.mockImplementationOnce(() => new Promise((r) => (release = () => r({}))));
		const pending = s.setIntensity('low');
		off();
		release();
		await pending;
		expect(s.undo).toBeUndefined();
		expect(vi.getTimerCount()).toBe(0);
	});
});
