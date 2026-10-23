import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { alertPath, feedPath, parseId, searchPath, torrentPath } from './api';
import { dividerBefore } from './since';
import { byteSpanToRange, saveFailure } from './errors';
import { throttled } from './throttle';

const r = (read: boolean, created_at = 0) => ({ read, created_at });

describe('dividerBefore', () => {
	it('sits between the last unread row and the first read row, labelled with that row', () => {
		expect(dividerBefore([r(false, 50), r(false, 40), r(true, 30), r(true, 20)])).toEqual({
			before: 2,
			at: 30
		});
	});
	it('waits for the first read row when every row so far is unread', () => {
		const rows = [r(false, 50), r(false, 40)];
		expect(dividerBefore(rows)).toBeNull();
		expect(dividerBefore([...rows, r(true, 30)])).toEqual({ before: 2, at: 30 });
	});
	it('is absent when nothing is unread, or when there are no rows', () => {
		expect(dividerBefore([r(true, 3), r(true, 2)])).toBeNull();
		expect(dividerBefore([])).toBeNull();
	});
});

describe('throttled', () => {
	afterEach(() => vi.useRealTimers());
	it('applies the first value at once and later ones at most once per window, last wins', () => {
		vi.useFakeTimers();
		const seen: string[] = [];
		const t = throttled<string>(
			10_000,
			(v) => seen.push(v),
			() => Date.now()
		);
		t.set('1 new alert');
		expect(seen).toEqual(['1 new alert']);
		vi.advanceTimersByTime(2_000);
		t.set('2 new alerts');
		vi.advanceTimersByTime(2_000);
		t.set('3 new alerts');
		expect(seen).toEqual(['1 new alert']);
		vi.advanceTimersByTime(6_000);
		expect(seen).toEqual(['1 new alert', '3 new alerts']);
	});
	it('reset applies at once without using up the window; cancel drops what waits', () => {
		vi.useFakeTimers();
		const seen: string[] = [];
		const t = throttled<string>(
			10_000,
			(v) => seen.push(v),
			() => Date.now()
		);
		t.reset('');
		t.set('a');
		expect(seen).toEqual(['', 'a']);
		t.set('b');
		t.cancel();
		vi.advanceTimersByTime(20_000);
		expect(seen).toEqual(['', 'a']);
	});
});

describe('byteSpanToRange', () => {
	it('is the identity for ASCII', () => {
		expect(byteSpanToRange('abc) def', 3, 4)).toEqual([3, 4]);
	});
	it('converts UTF-8 byte offsets to UTF-16 offsets', () => {
		// "é" is 2 bytes, "😀" is 4 bytes and 2 UTF-16 units.
		expect(byteSpanToRange('é😀)x', 6, 7)).toEqual([3, 4]);
	});
	it('clamps to the end', () => {
		expect(byteSpanToRange('ab', 5, 9)).toEqual([2, 2]);
	});
});

describe('saveFailure', () => {
	const err = (
		status: number,
		code: string,
		extra: ConstructorParameters<typeof ApiError>[3] = {}
	) => new ApiError(status, code, 'The parser says so.', extra);
	it('puts a syntax error on the query, with the server message and the span', () => {
		const f = saveFailure(err(400, 'query_syntax', { start: 2, end: 3 }), 'ab)c', 'en');
		expect(f).toMatchObject({ field: 'q', detail: 'The parser says so.', select: [2, 3] });
		expect(f.form).toBeUndefined();
	});
	it('maps invalid_value to the field the server names', () => {
		expect(saveFailure(err(422, 'invalid_value', { field: 'name' }), '', 'en').field).toBe('name');
		expect(saveFailure(err(422, 'invalid_value', { field: 'q' }), '', 'en').field).toBe('q');
		expect(saveFailure(err(422, 'invalid_value', { field: 'sort' }), '', 'en').form).toBeTruthy();
	});
	it('says how many saved searches Lodestar keeps', () => {
		expect(saveFailure(err(409, 'too_many_saved_searches'), '', 'en').form).toContain('1,000');
	});
	it('treats the codes that mean a bug, a network failure and a 5xx as form errors', () => {
		for (const [status, code] of [
			[400, 'q_and_ast'],
			[400, 'invalid_ast'],
			[400, 'unknown_sort'],
			[0, 'network'],
			[500, 'internal']
		] as const) {
			const f = saveFailure(err(status, code), '', 'en');
			expect(f.form, code).toBeTruthy();
			expect(f.field, code).toBeUndefined();
		}
		expect(saveFailure(new Error('x'), '', 'en').form).toBeTruthy();
	});
});

describe('paths', () => {
	it('builds the three data links and the feed path', () => {
		expect(alertPath(12)).toBe('/alerts/12');
		expect(feedPath(12)).toBe('/api/v1/feeds/12');
		expect(torrentPath('a'.repeat(40))).toBe(`/t/${'a'.repeat(40)}`);
	});
	it('links a torrent only for a 40-hex hash', () => {
		expect(torrentPath('A'.repeat(40))).toBeNull();
		expect(torrentPath('a'.repeat(39))).toBeNull();
		expect(torrentPath('../x')).toBeNull();
	});
	it('opens in Search with the query encoded, and the sort when not null', () => {
		expect(searchPath({ q: 'a b&c', sort: null })).toBe('/search?q=a+b%26c');
		expect(searchPath({ q: 'x', sort: '-discovered' })).toBe('/search?q=x&sort=-discovered');
	});
	it('accepts only positive integer ids', () => {
		expect(parseId('7')).toBe(7);
		for (const bad of ['0', '-1', '1.5', 'abc', '', '1e3', '99999999999999999999']) {
			expect(parseId(bad), bad).toBeNull();
		}
	});
});
