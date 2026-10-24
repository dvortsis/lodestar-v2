import { describe, expect, it } from 'vitest';
import { knownTab, searchHref, tabHref, torrentHref, typeParam } from './tabs';

const q = (s: string) => new URLSearchParams(s);

describe('tab URL state', () => {
	it('reads the tab from type: absent is All, a known type is itself, anything else is unknown', () => {
		expect(typeParam(q(''))).toBeNull();
		expect(typeParam(q('type='))).toBeNull();
		expect(knownTab(typeParam(q('type=movie')))).toBe('movie');
		expect(knownTab(null)).toBeNull();
		expect(knownTab('bogus')).toBeUndefined();
	});

	it('keeps density and other parameters on a tab link and drops the cursor', () => {
		expect(tabHref('tv', q('density=compact&cursor=abc&x=1'))).toBe(
			'/discover?density=compact&x=1&type=tv'
		);
		expect(tabHref(null, q('type=tv&density=compact'))).toBe('/discover?density=compact');
		expect(tabHref(null, q(''))).toBe('/discover');
	});

	it('links "See all in Search" to the query the server runs for Newest', () => {
		expect(searchHref('movie', q(''))).toBe('/search?q=type%3Amovie&sort=-discovered');
		expect(searchHref(null, q('density=compact'))).toBe('/search?sort=-discovered&density=compact');
	});

	it('builds /t/<hash> only from a 40-digit lowercase hex hash', () => {
		const h = 'a'.repeat(40);
		expect(torrentHref(h, q(''))).toBe(`/t/${h}`);
		expect(torrentHref(h, q('density=compact'))).toBe(`/t/${h}?density=compact`);
		for (const bad of ['', 'A'.repeat(40), 'a'.repeat(39), 'a'.repeat(41), `${'a'.repeat(39)}/`])
			expect(torrentHref(bad, q(''))).toBeNull();
	});
});
