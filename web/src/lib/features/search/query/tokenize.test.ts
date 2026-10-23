import { describe, expect, it } from 'vitest';
import { FIELDS, tokenize, type TokenKind } from './tokenize';

/** [kind, text] pairs, for readable expectations. */
const parts = (text: string) => tokenize(text).map((t) => [t.kind, text.slice(t.from, t.to)]);
const kinds = (text: string, kind: TokenKind) =>
	tokenize(text)
		.filter((t) => t.kind === kind)
		.map((t) => text.slice(t.from, t.to));

/** Small deterministic PRNG (mulberry32), so the property runs are reproducible. */
function rng(seed: number) {
	return () => {
		seed |= 0;
		seed = (seed + 0x6d2b79f5) | 0;
		let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

describe('tokenize', () => {
	it('gives each kind', () => {
		expect(parts('name:"big buck" size:>4GB res:1080p -cam | bunny')).toEqual([
			['field', 'name:'],
			['phrase', '"big buck"'],
			['text', ' '],
			['field', 'size:'],
			['range', '>4GB'],
			['text', ' '],
			['field', 'res:'],
			['value', '1080p'],
			['text', ' '],
			['operator', '-'],
			['text', 'cam '],
			['operator', '|'],
			['text', ' bunny']
		]);
	});

	it('reads ranges', () => {
		expect(parts('size:700MB..1.5GB')).toEqual([
			['field', 'size:'],
			['range', '700MB..1.5GB']
		]);
		expect(parts('discovered:<7d')).toEqual([
			['field', 'discovered:'],
			['range', '<7d']
		]);
		expect(kinds('cat:video>=80% year:2019..2021 files:<=3', 'range')).toEqual([
			'video>=80%',
			'2019..2021',
			'<=3'
		]);
		// No range operator: a value.
		expect(kinds('cat:software=0', 'value')).toEqual(['software=0']);
	});

	it('reads operators', () => {
		expect(parts('big NEAR/5 bunny')).toEqual([
			['text', 'big '],
			['operator', 'NEAR/5'],
			['text', ' bunny']
		]);
		expect(parts('-cam')).toEqual([
			['operator', '-'],
			['text', 'cam']
		]);
		expect(parts('!cam')).toEqual([
			['operator', '!'],
			['text', 'cam']
		]);
		expect(parts('(a | b) c')).toEqual([
			['operator', '('],
			['text', 'a '],
			['operator', '|'],
			['text', ' b'],
			['operator', ')'],
			['text', ' c']
		]);
		expect(parts('comput*')).toEqual([
			['text', 'comput'],
			['operator', '*']
		]);
		expect(parts('~bnuny')).toEqual([
			['operator', '~'],
			['text', 'bnuny']
		]);
		expect(kinds('a AND b OR c NEAR d', 'operator')).toEqual(['AND', 'OR', 'NEAR']);
	});

	it('mirrors the server: lowercase operators, a lone or inner minus, NOT are words', () => {
		expect(kinds('a and b or c', 'operator')).toEqual([]);
		expect(kinds('big NOT cam', 'operator')).toEqual([]);
		expect(kinds('WEB-DL', 'operator')).toEqual([]);
		expect(kinds('a - b', 'operator')).toEqual([]);
		expect(kinds('(a -)', 'operator')).toEqual(['(', ')']);
		expect(kinds('-|', 'operator')).toEqual(['|']);
	});

	it('reads fields with a negation, brackets and prefixes', () => {
		expect(parts('-type:adult')).toEqual([
			['operator', '-'],
			['field', 'type:'],
			['value', 'adult']
		]);
		expect(parts('file:(ext:flac)')).toEqual([
			['field', 'file:'],
			['operator', '('],
			['field', 'ext:'],
			['value', 'flac'],
			['operator', ')']
		]);
		expect(parts('path:subs*')).toEqual([
			['field', 'path:'],
			['value', 'subs'],
			['operator', '*']
		]);
	});

	it('knows the M4 swarm fields (the server answers "not available yet")', () => {
		for (const f of ['seeders', 'trackers', 'dht', 'seen', 'time', 'sig'])
			expect(FIELDS.has(f)).toBe(true);
		expect(kinds('seeders:>=1', 'field')).toEqual(['seeders:']);
	});

	it('runs an unterminated phrase to the end', () => {
		expect(parts('a "big buck')).toEqual([
			['text', 'a '],
			['phrase', '"big buck']
		]);
	});

	it('treats an unknown or capitalised field as text', () => {
		expect(parts('re:zero Size:>4GB')).toEqual([['text', 're:zero Size:>4GB']]);
		expect(parts(':x')).toEqual([['text', ':x']]);
	});

	it('handles CJK and emoji as words', () => {
		const text = '大白兔 name:🐰兔 "日本 語"';
		expect(parts(text)).toEqual([
			['text', '大白兔 '],
			['field', 'name:'],
			['value', '🐰兔'],
			['text', ' '],
			['phrase', '"日本 語"']
		]);
		// Non-ASCII whitespace is part of a word, as for the server.
		expect(parts('a　b')).toEqual([['text', 'a　b']]);
	});

	it('property: never throws, and the tokens cover the input in order', () => {
		const next = rng(42);
		const alphabet = [
			'a',
			'Z',
			' ',
			'\t',
			'"',
			'(',
			')',
			'|',
			'-',
			'!',
			'~',
			'*',
			':',
			'.',
			'>',
			'<',
			'=',
			'名',
			'🐰',
			'size:',
			'NEAR/',
			'OR'
		];
		for (let run = 0; run < 2000; run++) {
			let text = '';
			const len = Math.floor(next() * 40);
			for (let i = 0; i < len; i++) text += alphabet[Math.floor(next() * alphabet.length)];
			const toks = tokenize(text);
			let at = 0;
			for (const t of toks) {
				expect(t.from).toBe(at);
				expect(t.to).toBeGreaterThan(t.from);
				at = t.to;
			}
			expect(at).toBe(text.length);
			expect(toks.map((t) => text.slice(t.from, t.to)).join('')).toBe(text);
		}
	});

	it('is linear: 10 KB of hostile input tokenizes fast', () => {
		for (const unit of ['"a ', 'size:>1 ', '-(~a* | ', 'x'.repeat(50) + ' ']) {
			const text = unit.repeat(Math.ceil(10_240 / unit.length));
			const start = performance.now();
			const toks = tokenize(text);
			expect(performance.now() - start).toBeLessThan(50);
			expect(toks.map((t) => text.slice(t.from, t.to)).join('')).toBe(text);
		}
	});
});
