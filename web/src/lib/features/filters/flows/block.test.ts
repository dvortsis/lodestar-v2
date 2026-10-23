import { describe, expect, it } from 'vitest';
import { MAX_PHRASE_BYTES, nameWords, phraseQuery, suggestBlocks, torrentQuery } from './block';

const V1 = 'ab'.repeat(20);
const V2 = 'cd'.repeat(32);

describe('suggestBlocks', () => {
	it('lists the tiers narrowest first, with 2 and 3 unavailable and their reasons', () => {
		const s = suggestBlocks({ hash: V1, hash_v2: null, name: 'Some.Show.S01' });
		expect(s.map((x) => x.tier)).toEqual(['torrent', 'payload', 'group', 'phrase']);
		expect(s.map((x) => x.available)).toEqual([true, false, false, true]);
		expect(s[1]!.reason).toBe('no_payload_signature');
		expect(s[2]!.reason).toBe('no_group');
		expect(s[0]!.query).toBe(`hash:${V1}`);
		// The phrase is never preselected: it has no query until words are picked.
		expect(s[3]!.query).toBeUndefined();
	});

	it('a nameless torrent offers no phrase; a bad hash offers no torrent tier', () => {
		const s = suggestBlocks({ hash: 'nope', hash_v2: null, name: ' ..-- ' });
		expect(s[0]).toEqual({ tier: 'torrent', available: false });
		expect(s[3]!.available).toBe(false);
	});
});

describe('torrentQuery', () => {
	it('adds the truncated v2 hash, as the index keeps it', () => {
		expect(torrentQuery({ hash: V1, hash_v2: V2 })).toBe(`hash:${V1} | hash:${V2.slice(0, 40)}`);
		expect(torrentQuery({ hash: V1.toUpperCase(), hash_v2: null })).toBe(`hash:${V1}`);
		// A v2-only torrent's v1 key is its truncated v2 hash: one clause.
		expect(torrentQuery({ hash: V2.slice(0, 40), hash_v2: V2 })).toBe(`hash:${V2.slice(0, 40)}`);
	});
});

describe('nameWords and phraseQuery', () => {
	it('splits on whitespace and . _ - [ ] ( )', () => {
		expect(nameWords('[Grp] Some.Show_S01-E02 (1080p)')).toEqual([
			'Grp',
			'Some',
			'Show',
			'S01',
			'E02',
			'1080p'
		]);
	});

	it('strips quotes, collapses whitespace, and quotes the phrase', () => {
		expect(phraseQuery(['Some', '"Show"', ' S01 '])).toBe('name:"Some Show S01"');
		expect(phraseQuery(['a\t\nb'])).toBe('name:"a b"');
	});

	it('refuses empty and over-length phrases', () => {
		expect(phraseQuery([])).toBeNull();
		expect(phraseQuery(['""', '  '])).toBeNull();
		expect(phraseQuery(['x'.repeat(MAX_PHRASE_BYTES)])).toBe(
			`name:"${'x'.repeat(MAX_PHRASE_BYTES)}"`
		);
		expect(phraseQuery(['x'.repeat(MAX_PHRASE_BYTES + 1)])).toBeNull();
		// Bytes, not characters: 67 three-byte characters are 201 bytes.
		expect(phraseQuery(['字'.repeat(67)])).toBeNull();
	});
});
