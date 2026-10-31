import { describe, expect, it, vi } from 'vitest';
import { COMPARE_MAX, EXPORT_MAX, canCompare, copyMagnets, exportHashes } from './actions';

const reply = (body: string, init: ResponseInit = {}) =>
	vi.fn(async () => new Response(body, { status: 200, ...init })) as unknown as typeof fetch;

describe('Compare enablement', () => {
	it('takes two to five torrents', () => {
		expect([0, 1, 2, 5, 6].map(canCompare)).toEqual([false, false, true, true, false]);
		expect(COMPARE_MAX).toBe(5);
	});
});

describe('exportHashes', () => {
	it('posts the hashes and the format as JSON', async () => {
		const f = reply('hash\n', {
			headers: { 'Content-Disposition': 'attachment; filename="x.csv"' }
		});
		const r = await exportHashes(['a', 'b'], 'csv', f);
		expect(r).toEqual({ ok: true, text: 'hash\n', filename: 'x.csv' });
		const [url, init] = vi.mocked(f).mock.calls[0]!;
		expect(url).toBe('/api/v1/export');
		expect(JSON.parse(init!.body as string)).toEqual({ hashes: ['a', 'b'], format: 'csv' });
	});

	it('refuses more hashes than the server takes, without a request', async () => {
		const f = reply('');
		const r = await exportHashes(Array(EXPORT_MAX + 1).fill('a'), 'csv', f);
		expect(r).toEqual({ ok: false, reason: 'too_many' });
		expect(f).not.toHaveBeenCalled();
	});

	it('reports a refused or unreachable server as failed', async () => {
		expect(await exportHashes(['a'], 'csv', reply('', { status: 500 }))).toEqual({
			ok: false,
			reason: 'failed'
		});
		const down = vi.fn(async () => Promise.reject(new Error('offline'))) as unknown as typeof fetch;
		expect(await exportHashes(['a'], 'csv', down)).toEqual({ ok: false, reason: 'failed' });
	});
});

describe('copyMagnets', () => {
	it('copies the magnets newline-joined and counts them', async () => {
		const writes: string[] = [];
		Object.assign(navigator, {
			clipboard: { writeText: async (t: string) => void writes.push(t) }
		});
		Object.defineProperty(window, 'isSecureContext', { value: true, configurable: true });
		const r = await copyMagnets(
			['a', 'b'],
			reply('magnet:?xt=urn:btih:a\nmagnet:?xt=urn:btih:b\n')
		);
		expect(r).toMatchObject({ ok: true, count: 2, copied: true });
		expect(writes).toEqual(['magnet:?xt=urn:btih:a\nmagnet:?xt=urn:btih:b']);
	});

	it('reports a refused clipboard as not copied, with the text to show', async () => {
		Object.assign(navigator, {
			clipboard: { writeText: async () => Promise.reject(new Error('denied')) }
		});
		const r = await copyMagnets(
			['a', 'b'],
			reply('magnet:?xt=urn:btih:a\nmagnet:?xt=urn:btih:b\n')
		);
		expect(r).toMatchObject({ ok: true, copied: false, count: 2 });
		expect(r.ok && r.text).toBe('magnet:?xt=urn:btih:a\nmagnet:?xt=urn:btih:b');
	});

	it('fails when the server wrote no magnet', async () => {
		expect(await copyMagnets(['a'], reply('\n'))).toEqual({ ok: false, reason: 'failed' });
	});
});
