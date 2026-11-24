import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import {
	CONTENT_TYPE,
	MAX_BYTES,
	accepted,
	dataDirProblem,
	precheck,
	refused,
	send,
	tally
} from './upload';

const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
	new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json', ...headers }
	});
const envelope = (code: string) => ({ error: { code, message: 'server text' } });

afterEach(() => vi.unstubAllGlobals());

describe('precheck', () => {
	it('names the size limit before sending, and sends a file at the limit', () => {
		expect(precheck({ name: 'a.torrent', size: MAX_BYTES + 1 })).toEqual({ state: 'too_large' });
		expect(precheck({ name: 'a.torrent', size: MAX_BYTES })).toBeUndefined();
		expect(precheck({ name: 'A.TORRENT', size: 10 })).toBeUndefined();
	});

	it('an empty file, or one not named .torrent, is not a torrent file', () => {
		expect(precheck({ name: 'a.torrent', size: 0 })).toEqual({ state: 'malformed' });
		expect(precheck({ name: 'a.txt', size: 10 })).toEqual({ state: 'malformed' });
		expect(precheck({ name: 'a.torrent.exe', size: 10 })).toEqual({ state: 'malformed' });
	});
});

describe('answers', () => {
	it('a 202 is added; a hash that is not 40 hex digits is dropped', () => {
		const hash = 'AB'.repeat(20);
		expect(accepted({ info_hash: hash, private: true })).toEqual({
			state: 'added',
			hash: hash.toLowerCase(),
			private: true
		});
		expect(accepted({ info_hash: '../../x', private: 'yes' })).toEqual({
			state: 'added',
			hash: '',
			private: false
		});
		expect(accepted(null)).toEqual({ state: 'added', hash: '', private: false });
	});

	it('maps each refusal by its code, never its message', () => {
		const e = (code: string, retryAfter?: number) =>
			new ApiError(400, code, 'whatever', { retryAfter });
		expect(refused(e('malformed_torrent'))).toEqual({ state: 'malformed' });
		expect(refused(e('held'))).toEqual({ state: 'held' });
		expect(refused(e('refused'))).toEqual({ state: 'refused' });
		expect(refused(e('body_too_large'))).toEqual({ state: 'too_large' });
		expect(refused(e('rate_limited', 7))).toEqual({ state: 'rate_limited', wait: 7 });
		expect(refused(e('uploads_unavailable'))).toEqual({ state: 'unavailable' });
		for (const code of ['body_timeout', 'cross_origin', 'bad_body', 'http_502', 'network'])
			expect(refused(e(code))).toEqual({ state: 'failed' });
	});

	it('tallies by state', () => {
		expect(tally([{ state: 'added' }, { state: 'added' }, { state: 'held' }])).toEqual({
			added: 2,
			held: 1
		});
	});
});

describe('send', () => {
	it('posts the raw file with only the fixed headers: no name, no multipart, no redirect', async () => {
		const fetch = vi.fn(async () => json(202, { info_hash: 'a'.repeat(40), private: false }));
		vi.stubGlobal('fetch', fetch);
		const file = new File([new Uint8Array([100, 101])], 'secret name.torrent');
		expect(await send(file)).toEqual({ state: 'added', hash: 'a'.repeat(40), private: false });
		expect(fetch).toHaveBeenCalledOnce();
		const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
		expect(url).toBe('/api/v1/torrents/upload');
		expect(init.method).toBe('POST');
		expect(init.headers).toEqual({ 'Content-Type': CONTENT_TYPE, Accept: 'application/json' });
		expect(init.body).toBe(file);
		expect(init.credentials).toBe('same-origin');
		expect(init.redirect).toBe('error');
		expect(JSON.stringify(init)).not.toContain('secret name');
	});

	it('reads refusals, Retry-After included', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => json(429, envelope('rate_limited'), { 'Retry-After': '12' }))
		);
		expect(await send(new Blob(['x']))).toEqual({ state: 'rate_limited', wait: 12 });
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => json(422, envelope('held')))
		);
		expect(await send(new Blob(['x']))).toEqual({ state: 'held' });
	});

	it('a network failure or a proxy page is a failure to retry', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('Failed to fetch');
			})
		);
		expect(await send(new Blob(['x']))).toEqual({ state: 'failed' });
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response('<html>bad gateway</html>', { status: 502 }))
		);
		expect(await send(new Blob(['x']))).toEqual({ state: 'failed' });
	});

	it('rejects when aborted', async () => {
		const ctl = new AbortController();
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				ctl.abort();
				throw new DOMException('Aborted', 'AbortError');
			})
		);
		await expect(send(new Blob(['x']), ctl.signal)).rejects.toMatchObject({ name: 'AbortError' });
	});
});

describe('dataDirProblem', () => {
	it('finds a folder inside, equal to or holding the data folder', () => {
		expect(dataDirProblem('/data', '/data')).toBe('inside');
		expect(dataDirProblem('/data/watch/', '/data')).toBe('inside');
		expect(dataDirProblem('//data/./watch', '/data/')).toBe('inside');
		expect(dataDirProblem('/', '/data')).toBe('holds');
		expect(dataDirProblem('/srv', '/srv/lodestar/data')).toBe('holds');
	});

	it('compares whole parts, not text', () => {
		expect(dataDirProblem('/data2', '/data')).toBeUndefined();
		expect(dataDirProblem('/dat', '/data')).toBeUndefined();
		expect(dataDirProblem('/watch', '/data')).toBeUndefined();
	});

	it('Windows paths ignore case and take either slash', () => {
		expect(dataDirProblem('c:/ProgramData/Lodestar/watch', 'C:\\ProgramData\\Lodestar')).toBe(
			'inside'
		);
		expect(dataDirProblem('D:\\Torrents', 'C:\\ProgramData\\Lodestar')).toBeUndefined();
		expect(dataDirProblem('\\\\nas\\share', '\\\\NAS\\share\\lodestar')).toBe('holds');
	});

	it('says nothing it cannot know without the disk', () => {
		expect(dataDirProblem('', '/data')).toBeUndefined();
		expect(dataDirProblem('relative/watch', '/data')).toBeUndefined();
		expect(dataDirProblem('/data/../watch', '/data')).toBeUndefined();
		expect(dataDirProblem('/watch', '')).toBeUndefined();
		expect(dataDirProblem('C:\\data', '/data')).toBeUndefined();
	});
});
