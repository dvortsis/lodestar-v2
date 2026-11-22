import { describe, expect, it } from 'vitest';
import { readUrl } from '$lib/state/search.svelte';
import { SORTS, VIEWS, prefOf, sortValue, viewHref, viewOf, viewUnset } from './toolbar';

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

describe('viewHref', () => {
	it('sets one view, drops the default, and keeps everything else', () => {
		const u = new URL('http://x/search?q=a&sort=-size#top');
		expect(viewHref(u, 'compact')).toBe('/search?q=a&sort=-size&density=compact#top');
		expect(viewHref(u, 'cards')).toBe('/search?q=a&sort=-size&density=cards#top');
		expect(viewHref(u, 'table')).toBe('/search?q=a&sort=-size&view=table#top');
		const c = new URL('http://x/search?density=compact&view=table&q=a');
		expect(viewHref(c, 'list')).toBe('/search?q=a');
		expect(viewHref(c, 'cards')).toBe('/search?q=a&density=cards');
	});
	it('round-trips every view', () => {
		for (const v of VIEWS)
			expect(viewOf(new URL(viewHref(new URL('http://x/search?q=a'), v), 'http://x'))).toBe(v);
	});
});

describe('viewOf: every old density and view combination lands on one option', () => {
	const of = (q: string) => viewOf(new URL('http://x/search?' + q));
	it.each([
		['', 'list'],
		['density=comfortable', 'list'],
		['density=bogus', 'list'],
		['density=compact', 'compact'],
		['density=cards', 'cards'],
		['view=list', 'list'],
		['view=table', 'table'],
		['view=table&density=compact', 'table'],
		['view=table&density=cards', 'table'],
		['view=list&density=cards', 'cards'],
		['view=list&density=compact', 'compact']
	])('%s -> %s', (q, v) => expect(of(q)).toBe(v));
});

describe('the default view: the URL first, then the account preference, then the List', () => {
	const of = (q: string, pref?: 'compact' | 'cards' | 'comfortable') =>
		viewOf(new URL('http://x/search?' + q), pref);
	it('uses the preference only when the URL names no view', () => {
		expect(of('q=a')).toBe('list');
		expect(of('q=a', 'compact')).toBe('compact');
		expect(of('q=a', 'cards')).toBe('cards');
		expect(of('q=a&density=bogus', 'cards')).toBe('cards');
	});
	it('lets an explicit view or density win over the preference', () => {
		expect(of('q=a&view=table', 'cards')).toBe('table');
		expect(of('q=a&density=compact', 'cards')).toBe('compact');
		expect(of('q=a&density=comfortable', 'cards')).toBe('list');
		expect(of('q=a&density=cards', 'compact')).toBe('cards');
	});
	it('maps the preference one to one, and reads anything else as comfortable', () => {
		expect(prefOf('compact')).toBe('compact');
		expect(prefOf('cards')).toBe('cards');
		expect(prefOf('comfortable')).toBe('comfortable');
		expect(prefOf(undefined)).toBe('comfortable');
		expect(prefOf('x')).toBe('comfortable');
	});
	it('says when the URL is silent, and writes the List out when the preference is not List', () => {
		expect(viewUnset(new URL('http://x/search?q=a'))).toBe(true);
		expect(viewUnset(new URL('http://x/search?q=a&density=comfortable'))).toBe(false);
		expect(viewUnset(new URL('http://x/search?q=a&view=table'))).toBe(false);
		const u = new URL('http://x/search?q=a&density=cards');
		expect(viewHref(u, 'list', 'cards')).toBe('/search?q=a&density=comfortable');
		expect(viewHref(u, 'list', 'comfortable')).toBe('/search?q=a');
	});
});
