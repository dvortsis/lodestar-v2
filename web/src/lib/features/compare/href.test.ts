import { describe, expect, it } from 'vitest';
import { compareHref, parseHashes, torrentHref } from './href';

describe('href', () => {
	it('keeps the order, drops empties, validates nothing else', () => {
		expect(parseHashes('b,a,,c,')).toEqual(['b', 'a', 'c']);
		expect(parseHashes('')).toEqual([]);
		expect(parseHashes(null)).toEqual([]);
		expect(parseHashes('zz')).toEqual(['zz']);
	});

	it('writes /compare?h=a,b and keeps other parameters', () => {
		expect(compareHref(['a', 'b'])).toBe('/compare?h=a,b');
		expect(compareHref(['a'], new URLSearchParams('h=x,y&density=compact'))).toBe(
			'/compare?h=a&density=compact'
		);
	});

	it('links only well-formed hashes', () => {
		const hash = 'ab'.repeat(20);
		expect(torrentHref(hash)).toBe(`/t/${hash}`);
		expect(torrentHref('AB'.repeat(20))).toBeNull();
		expect(torrentHref('../x')).toBeNull();
		expect(torrentHref(hash + 'a')).toBeNull();
	});
});
