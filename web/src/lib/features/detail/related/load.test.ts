import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { clearDetailCaches } from '../cache';
import { HASH } from '../fixtures';
import type { Loaded } from './load';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (original) => ({
	...(await original<typeof import('$lib/api/client')>()),
	api: { get }
}));
const { loadSiblings } = await import('./load');

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
});
