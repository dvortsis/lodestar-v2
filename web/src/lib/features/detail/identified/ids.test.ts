import { describe, expect, it } from 'vitest';
import { idLink } from './ids';

describe('idLink', () => {
	it('builds a fixed https link from a well-formed id', () => {
		expect(idLink('imdb', 'tt0111161')).toEqual({
			href: 'https://www.imdb.com/title/tt0111161/',
			name: 'IMDb'
		});
		expect(idLink('wikidata', 'Q172241')?.href).toBe('https://www.wikidata.org/wiki/Q172241');
	});
	it('refuses a malicious or malformed id', () => {
		for (const bad of [
			'tt0111161/../../x',
			'javascript:alert(1)',
			'tt0111161"onmouseover="x',
			'https://evil.example/',
			'tt12',
			' tt0111161',
			'tt0111161\n'
		])
			expect(idLink('imdb', bad)).toBeNull();
		expect(idLink('anidb', '1/../../admin')).toBeNull();
	});
	it('gives an unknown or inherited scheme no link', () => {
		expect(idLink('unknown', '123')).toBeNull();
		expect(idLink('tmdb', '123')).toBeNull();
		expect(idLink('constructor', 'x')).toBeNull();
		expect(idLink('__proto__', 'x')).toBeNull();
	});
});
