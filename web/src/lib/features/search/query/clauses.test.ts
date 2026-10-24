import { describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { chipClauses, isFreeText, label, rootWithout, topLevel, withoutFilters } from './clauses';

const G = 1024 ** 3;
const M = 1024 ** 2;
const nb = ' '; // formatBytes puts a no-break space before the unit
const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const res = (v: string): Ast => ({ op: 'tag', key: 'res', any: [v] });
const text = (n: Ast) => label(n).text;
const HASH = '0123456789abcdef0123456789abcdef01234567';

describe('labels: every AST op', () => {
	it('all, and, or, not, file', () => {
		expect(text({ op: 'all' })).toBe('Everything');
		expect(text({ op: 'and', of: [res('1080p'), { op: 'ext', any: ['mkv'] }] })).toBe(
			'All of: Resolution: 1080p and Extension: .mkv'
		);
		expect(text({ op: 'or', of: [res('1080p'), res('2160p')] })).toBe(
			'Any of: Resolution: 1080p or Resolution: 2160p'
		);
		expect(text({ op: 'not', of: word('cam') })).toBe('Not: cam');
		expect(
			text({
				op: 'file',
				of: {
					op: 'and',
					of: [
						{ op: 'ext', any: ['flac'] },
						{ op: 'range', field: 'size', lo: 50 * M + 1, hi: null }
					]
				}
			})
		).toBe(`A file matching: All of: Extension: .flac and Size: at least 50.0${nb}MiB`);
	});

	it('word, phrase, near, prefix, fuzzy (scoped to a field)', () => {
		expect(text({ op: 'word', field: 'name', word: 'bunny' })).toBe('Raw name: bunny');
		expect(text({ op: 'phrase', field: 'title', text: 'big buck bunny' })).toBe(
			'Resolved title: "big buck bunny"'
		);
		expect(text({ op: 'near', field: 'name', slop: 5, words: ['big', 'bunny'] })).toBe(
			'Raw name: big NEAR/5 bunny'
		);
		expect(text({ op: 'prefix', field: 'path', prefix: 'subs' })).toBe('File paths: subs*');
		expect(text({ op: 'fuzzy', field: 'title', word: 'squirel', distance: 2 })).toBe(
			'Resolved title: ~squirel'
		);
	});

	it('range: bytes, counts, scores and plain identifiers', () => {
		expect(text({ op: 'range', field: 'size', lo: 4 * G + 1, hi: null })).toBe(
			`Size: at least 4.00${nb}GiB`
		);
		expect(text({ op: 'range', field: 'size', lo: 700 * M, hi: 1.5 * G })).toBe(
			`Size: 700${nb}MiB to 1.50${nb}GiB`
		);
		expect(text({ op: 'range', field: 'largest', lo: null, hi: 2 * G })).toBe(
			`Largest file: at most 2.00${nb}GiB`
		);
		expect(text({ op: 'range', field: 'piece', lo: 16 * M, hi: 16 * M })).toBe(
			`Piece size: 16.0${nb}MiB`
		);
		expect(text({ op: 'range', field: 'files', lo: 10_000, hi: null })).toBe(
			'Files: at least 10,000'
		);
		expect(text({ op: 'range', field: 'junk', lo: null, hi: 19 })).toBe('Junk score: at most 19');
		// Identifiers: never grouped.
		expect(text({ op: 'range', field: 'year', lo: 2019, hi: 2021 })).toBe('Year: 2019 to 2021');
		expect(text({ op: 'range', field: 'season', lo: 2, hi: 2 })).toBe('Season: 2');
		expect(text({ op: 'range', field: 'episode', lo: 1000, hi: null })).toBe(
			'Episode: at least 1000'
		);
		expect(text({ op: 'range', field: 'size', lo: null, hi: null })).toBe('Size: any');
	});

	it('date: ages and UTC days', () => {
		const d = (from: unknown, to: unknown) =>
			text({ op: 'date', field: 'discovered', from, to } as Ast);
		const jan1 = 1767225600; // 2026-01-01T00:00:00Z
		expect(d({ ago: 7 * 86_400 }, null)).toBe('Discovered in the last 7 days');
		expect(d(null, { ago: 14 * 86_400 })).toBe('Discovered more than 14 days ago');
		expect(d({ at: jan1 }, { at: jan1 + 86_399 })).toBe('Discovered on Jan 1, 2026');
		expect(d({ at: jan1 }, { at: 1774915200 + 86_399 })).toBe(
			'Discovered Jan 1, 2026 to Mar 31, 2026'
		);
		expect(d({ at: jan1 + 86_400 }, null)).toBe('Discovered since Jan 2, 2026');
		expect(d(null, { at: jan1 + 86_399 })).toBe('Discovered until Jan 1, 2026');
		expect(d({ ago: 86_400 }, { at: jan1 })).toBe('Discovered 24 hr ago to Jan 1, 2026');
	});

	it('ext, flag, hash, tag, share', () => {
		expect(text({ op: 'ext', any: ['mkv', '.mp4'] })).toBe('Extension: .mkv, .mp4');
		expect(text({ op: 'flag', name: 'private', value: false })).toBe('Private: no');
		expect(text({ op: 'flag', name: 'hidden', value: true })).toBe('Hidden by filters: yes');
		expect(text({ op: 'flag', name: 'files_partial', value: true })).toBe('Partly indexed: yes');
		expect(text({ op: 'flag', name: 'someday', value: true })).toBe('someday: yes');
		expect(text({ op: 'hash', hash: HASH })).toBe(`Info hash: ${HASH}`);
		expect(text({ op: 'tag', key: 'type', any: ['movie', 'tv'] })).toBe('Type: Movie, TV');
		expect(text({ op: 'tag', key: 'adult', any: ['vr'] })).toBe('Adult type: VR');
		expect(text({ op: 'tag', key: 'codec', any: ['h265'] })).toBe('Video codec: h265');
		// A bare `cat:video` (1–100%) reads as "has some"; `bytes:` keeps its percentage.
		expect(text({ op: 'share', by: 'count', cat: 'video', lo: 1, hi: 100 })).toBe(
			'Has Video files'
		);
		expect(text({ op: 'share', by: 'count', cat: 'video', lo: 80, hi: 100 })).toBe(
			'Video files: at least 80%'
		);
		expect(text({ op: 'share', by: 'bytes', cat: 'software', lo: 0, hi: 5 })).toBe(
			'Software by size: at most 5%'
		);
		expect(text({ op: 'share', by: 'count', cat: 'audio', lo: 10, hi: 50 })).toBe(
			'Audio files: 10% to 50%'
		);
		expect(text({ op: 'share', by: 'count', cat: 'software', lo: 0, hi: 0 })).toBe(
			'Software files: 0%'
		);
	});

	it('shows values in mono, the hash shortened with the full hash in the text', () => {
		expect(label({ op: 'tag', key: 'codec', any: ['h265'] }).parts).toEqual([
			'Video codec',
			': ',
			{ mono: 'h265' }
		]);
		// Type and adult values are labels, not codes: no mono.
		expect(label({ op: 'tag', key: 'type', any: ['movie'] }).parts).toEqual([
			'Type',
			': ',
			'Movie'
		]);
		expect(label({ op: 'hash', hash: HASH }).parts).toEqual([
			'Info hash',
			': ',
			{ mono: '0123456789ab…', full: HASH }
		]);
		// An unknown type value (a newer server) shows as the server wrote it.
		expect(label({ op: 'tag', key: 'type', any: ['podcast'] }).parts.at(-1)).toEqual({
			mono: 'podcast'
		});
	});

	it('limits nesting and cuts long labels, keeping the whole text', () => {
		let deep: Ast = res('1080p');
		for (let i = 0; i < 6; i++) deep = { op: 'not', of: deep };
		expect(text(deep)).toBe('Not: Not: Not: Not: …');
		const many: Ast = { op: 'ext', any: Array.from({ length: 30 }, (_, i) => `ext${i}`) };
		const l = label(many);
		const shown = l.parts.map((p) => (typeof p === 'string' ? p : p.mono)).join('');
		expect(shown.length).toBeLessThanOrEqual(81);
		expect(shown.endsWith('…')).toBe(true);
		expect(l.text).toContain('.ext29');
		// The cut never splits an emoji (a surrogate pair): "Release group: " + 64 x lands mid-emoji.
		const emoji = label({ op: 'tag', key: 'group', any: ['x'.repeat(64) + '😀'.repeat(10)] });
		const cut = emoji.parts.map((p) => (typeof p === 'string' ? p : p.mono)).join('');
		expect(cut.endsWith('…')).toBe(true);
		expect(/[\uD800-\uDBFF]…$/.test(cut)).toBe(false);
	});

	it('swarm counts, last seen and the time of day (#48)', () => {
		expect(text({ op: 'range', field: 'seeders', lo: 5, hi: null })).toBe('Seeders: at least 5');
		expect(text({ op: 'range', field: 'leechers', lo: 1, hi: null })).toBe('Leechers: at least 1');
		expect(text({ op: 'range', field: 'trackers', lo: 3, hi: null })).toBe(
			'Trackers reporting: at least 3'
		);
		expect(text({ op: 'range', field: 'dht', lo: 1200, hi: null })).toBe(
			'DHT estimate: at least 1,200'
		);
		expect(text({ op: 'date', field: 'seen', from: { ago: 86_400 }, to: null })).toBe(
			'Last seen in the last 24 hr'
		);
		expect(text({ op: 'time_of_day', from: 22 * 60, to: 2 * 60 })).toBe(
			'Discovered between 22:00 and 02:00 UTC'
		);
		expect(text({ op: 'time_of_day', from: 9 * 60, to: 17 * 60 + 30 })).toBe(
			'Discovered between 09:00 and 17:30 UTC'
		);
	});

	it('has a fallback for an op this client does not know', () => {
		expect(text({ op: 'sig' } as unknown as Ast)).toBe('Filter');
	});
});

describe('which clauses get a chip', () => {
	it('free text stays in the bar', () => {
		expect(isFreeText(word('bunny'))).toBe(true);
		expect(isFreeText({ op: 'phrase', field: 'any', text: 'a b' })).toBe(true);
		expect(isFreeText({ op: 'near', field: 'any', slop: 5, words: ['a', 'b'] })).toBe(true);
		expect(isFreeText({ op: 'prefix', field: 'any', prefix: 'co' })).toBe(true);
		expect(isFreeText({ op: 'fuzzy', field: 'any', word: 'abc', distance: 1 })).toBe(true);
		expect(isFreeText({ op: 'or', of: [word('a'), word('b')] })).toBe(true);
		expect(isFreeText({ op: 'or', of: [word('a'), res('1080p')] })).toBe(false);
		expect(isFreeText({ op: 'word', field: 'name', word: 'a' })).toBe(false);
		expect(isFreeText({ op: 'not', of: word('cam') })).toBe(false);
	});

	it('top-level clauses: a root and, else the root; all has none', () => {
		expect(topLevel({ op: 'all' })).toEqual([]);
		expect(topLevel(null)).toEqual([]);
		expect(topLevel(res('1080p'))).toEqual([res('1080p')]);
		expect(topLevel({ op: 'and', of: [word('a'), res('1080p')] })).toHaveLength(2);
	});

	it('chips keep their top-level position', () => {
		const ast: Ast = {
			op: 'and',
			of: [word('big'), res('1080p'), word('bunny'), { op: 'ext', any: ['mkv'] }]
		};
		expect(chipClauses(ast).map((c) => [c.index, c.label.text])).toEqual([
			[1, 'Resolution: 1080p'],
			[3, 'Extension: .mkv']
		]);
		expect(chipClauses(word('x'))).toEqual([]);
	});
});

describe('removing', () => {
	const ast: Ast = { op: 'and', of: [word('lantern'), res('1080p'), { op: 'ext', any: ['mkv'] }] };

	it('drops one clause and keeps the rest in order', () => {
		expect(rootWithout(ast, 1)).toEqual({
			op: 'and',
			of: [word('lantern'), { op: 'ext', any: ['mkv'] }]
		});
	});

	it('unwraps a single remaining clause', () => {
		expect(rootWithout({ op: 'and', of: [word('lantern'), res('1080p')] }, 1)).toEqual(
			word('lantern')
		);
	});

	it('the last one becomes all', () => {
		expect(rootWithout(res('1080p'), 0)).toEqual({ op: 'all' });
	});

	it('Clear all keeps the free text', () => {
		expect(withoutFilters(ast)).toEqual(word('lantern'));
		expect(
			withoutFilters({ op: 'and', of: [res('1080p'), { op: 'not', of: word('cam') }] })
		).toEqual({
			op: 'all'
		});
		expect(withoutFilters({ op: 'and', of: [word('a'), res('1080p'), word('b')] })).toEqual({
			op: 'and',
			of: [word('a'), word('b')]
		});
	});
});
