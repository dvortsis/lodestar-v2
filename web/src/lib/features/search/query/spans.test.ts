import { describe, expect, it } from 'vitest';
import { byteLength, byteToUtf16, errorRange } from './spans';

describe('byteToUtf16', () => {
	it('maps ASCII one to one', () => {
		expect(byteToUtf16('size:big', 5)).toBe(5);
	});

	it('maps CJK (3 bytes, 1 unit) and emoji (4 bytes, 2 units)', () => {
		const text = '大白兔 x';
		expect(byteToUtf16(text, 3)).toBe(1);
		expect(byteToUtf16(text, 9)).toBe(3);
		expect(byteToUtf16(text, 10)).toBe(4);
		const emoji = '🐰🐰x';
		expect(byteToUtf16(emoji, 4)).toBe(2);
		expect(byteToUtf16(emoji, 8)).toBe(4);
		expect(byteLength(emoji)).toBe(9);
	});

	it('clamps out-of-range offsets', () => {
		expect(byteToUtf16('abc', -4)).toBe(0);
		expect(byteToUtf16('abc', Number.NaN)).toBe(0);
		expect(byteToUtf16('abc', 99)).toBe(3);
	});

	it('moves an offset inside a code point to its start', () => {
		expect(byteToUtf16('大x', 1)).toBe(0);
		expect(byteToUtf16('大x', 2)).toBe(0);
		expect(byteToUtf16('a🐰', 3)).toBe(1);
	});
});

describe('errorRange', () => {
	it('converts the server span', () => {
		// `size:big` → the value "big" is bytes 5..8.
		expect(errorRange('size:big', 5, 8)).toEqual({ from: 5, to: 8 });
		expect(errorRange('大 size:big', 9, 12)).toEqual({ from: 7, to: 10 });
	});

	it('marks one character for an empty span, a whole emoji included', () => {
		expect(errorRange('a)b', 1, 1)).toEqual({ from: 1, to: 2 });
		expect(errorRange('x🐰', 1, 1)).toEqual({ from: 1, to: 3 });
	});

	it('stays empty at the end of the text', () => {
		expect(errorRange('(a', 2, 2)).toEqual({ from: 2, to: 2 });
		expect(errorRange('(a', 50, 50)).toEqual({ from: 2, to: 2 });
	});

	it('never runs backwards and tolerates a missing span', () => {
		expect(errorRange('abc', 2, 1)).toEqual({ from: 2, to: 3 });
		expect(errorRange('abc', undefined, undefined)).toEqual({ from: 0, to: 1 });
	});
});
