import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { clearDetailCaches } from './cache';
import { HASH } from './fixtures';
import type { Loaded } from './siblings';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));
const { loadSiblings } = await import('./siblings');

const pending = () => {
	let resolve!: (v: unknown) => void;
	let reject!: (e: unknown) => void;
	get.mockImplementation((_path: string, opts: { signal: AbortSignal }) => {
		return new Promise((res, rej) => {
			resolve = res;
			reject = rej;
			opts.signal.addEventListener('abort', () => rej(new DOMException('aborted', 'AbortError')));
		});
	});
	return { resolve: (v: unknown) => resolve(v), reject: (e: unknown) => reject(e) };
};
const settle = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
	get.mockReset();
	clearDetailCaches(false); // nothing kept from the test before
});

describe('loadSiblings', () => {
	it('reports loading, then the answer', async () => {
		const p = pending();
		const states: Loaded['status'][] = [];
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		p.resolve({ hash: HASH });
		await settle();
		expect(states).toEqual(['loading', 'ready']);
	});

	it('turns a 404 into withheld and anything else into failed', async () => {
		let p = pending();
		const states: Loaded['status'][] = [];
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		p.reject(new ApiError(404, 'torrent_not_found', ''));
		await settle();
		p = pending();
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		p.reject(new ApiError(503, 'unavailable', ''));
		await settle();
		expect(states).toEqual(['loading', 'withheld', 'loading', 'failed']);
	});

	it('drops an answer once cancelled or once the content settings changed', async () => {
		let p = pending();
		const states: Loaded['status'][] = [];
		const cancel = loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		cancel();
		p.resolve({ hash: HASH });
		await settle();
		p = pending();
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		clearDetailCaches(false);
		p.resolve({ hash: HASH });
		await settle();
		expect(states).toEqual(['loading', 'loading']);
	});

	it('treats an answer about another torrent as a failure, and keeps nothing of it', async () => {
		let p = pending();
		const states: Loaded['status'][] = [];
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		p.resolve({ hash: 'b'.repeat(40) });
		await settle();
		p = pending();
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		expect(get).toHaveBeenCalledTimes(2);
		expect(states).toEqual(['loading', 'failed', 'loading']);
	});

	it('shares one request between the sections, and keeps the answer for the next one', async () => {
		const p = pending();
		const a: Loaded['status'][] = [];
		const b: Loaded['status'][] = [];
		loadSiblings(HASH, (s) => a.push(s.status));
		const stopB = loadSiblings(HASH, (s) => b.push(s.status));
		await settle();
		// One leaving doesn't cancel the other's load.
		stopB();
		loadSiblings(HASH, (s) => b.push(s.status));
		p.resolve({ hash: HASH });
		await settle();
		expect(get).toHaveBeenCalledOnce();
		expect(a).toEqual(['loading', 'ready']);
		expect(b).toEqual(['loading', 'loading', 'ready']);
		// Mounted later: the kept answer, no request.
		const c: Loaded['status'][] = [];
		loadSiblings(HASH, (s) => c.push(s.status));
		expect(c).toEqual(['ready']);
		expect(get).toHaveBeenCalledOnce();
	});

	it('asks again on Retry, and after a content-settings change', async () => {
		let p = pending();
		loadSiblings(HASH, () => {});
		await settle();
		p.resolve({ hash: HASH });
		await settle();
		p = pending();
		const states: Loaded['status'][] = [];
		loadSiblings(HASH, (s) => states.push(s.status), true);
		await settle();
		p.resolve({ hash: HASH });
		await settle();
		clearDetailCaches(false);
		p = pending();
		loadSiblings(HASH, (s) => states.push(s.status));
		await settle();
		expect(get).toHaveBeenCalledTimes(3);
		expect(states).toEqual(['loading', 'ready', 'loading']);
	});

	it('cancels the request once nobody waits for it', async () => {
		pending();
		const stop = loadSiblings(HASH, () => {});
		await settle();
		const signal = (get.mock.calls[0]![1] as { signal: AbortSignal }).signal;
		expect(signal.aborted).toBe(false);
		stop();
		expect(signal.aborted).toBe(true);
	});
});
