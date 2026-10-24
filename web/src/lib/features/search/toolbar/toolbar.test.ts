import { describe, expect, it } from 'vitest';
import { readUrl } from '$lib/state/search.svelte';
import { SORTS, densityHref, sortValue } from './toolbar';

describe('the toolbar’s sorts', () => {
	it('are all ones W5 keeps in the URL, each field both ways', () => {
		for (const s of SORTS)
			expect(readUrl(new URLSearchParams({ sort: s.value })).sort).toBe(s.value);
		expect(SORTS.map((s) => s.value)).toEqual([
			'relevance',
			'-discovered',
			'+discovered',
			'-size',
			'+size',
			'-files',
			'+files'
		]);
	});

	it('read a bare field as descending, and anything else as relevance', () => {
		expect(sortValue('discovered')).toBe('-discovered');
		expect(sortValue('+size')).toBe('+size');
		expect(sortValue('relevance')).toBe('relevance');
		expect(sortValue('seeders')).toBe('relevance');
	});
});

describe('densityHref', () => {
	it('sets compact, drops the default, and keeps everything else', () => {
		const u = new URL('http://x/search?q=a&sort=-size#top');
		expect(densityHref(u, 'compact')).toBe('/search?q=a&sort=-size&density=compact#top');
		const c = new URL('http://x/search?density=compact&q=a');
		expect(densityHref(c, 'comfortable')).toBe('/search?q=a');
	});
});
