import { describe, expect, it } from 'vitest';
import { parseToken } from './token';

const T = 'AB12-CD34-EF56-7890';
const CANONICAL = /^[0-9A-F]{4}(-[0-9A-F]{4}){3}$/;

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
const pick = <T>(r: () => number, xs: readonly T[]) => xs[Math.floor(r() * xs.length)]!;

describe('parseToken', () => {
	it.each([
		['canonical', T],
		['lower case', 'ab12-cd34-ef56-7890'],
		['no separators', 'AB12CD34EF567890'],
		['spaces', 'AB12 CD34 EF56 7890'],
		['surrounding whitespace and newline', '  AB12-CD34-EF56-7890\n'],
		['en dashes', 'AB12–CD34–EF56–7890'],
		['quotes', '"AB12-CD34-EF56-7890"'],
		['full-width (CJK input)', 'ＡＢ１２－ＣＤ３４－ＥＦ５６－７８９０'],
		[
			'the whole log line',
			'2026-10-22T10:00:00Z WARN lodestar: first run: open http://0.0.0.0:8090/ and enter setup token AB12-CD34-EF56-7890 to create the admin password'
		],
		['mixed separators', 'ab12_cd34.ef56 7890']
	])('accepts %s', (_, input) => {
		expect(parseToken(input)).toBe(T);
	});

	it.each([
		['empty', ''],
		['too short', 'AB12-CD34-EF56-789'],
		['too long', 'AB12-CD34-EF56-78901'],
		['not hex', 'AB12-CD34-EF56-789G'],
		['two different tokens', 'AB12-CD34-EF56-7890 and 0000-1111-2222-3333'],
		['a token glued to more hex', 'FFAB12-CD34-EF56-7890'],
		['words only', 'setup token']
	])('rejects %s', (_, input) => {
		expect(parseToken(input)).toBeNull();
	});

	it('accepts the same token pasted twice', () => {
		expect(parseToken(`${T} ${T}`)).toBe(T);
	});

	it('property: any token survives case, separators and padding', () => {
		const r = rng(1);
		const seps = ['', '-', ' ', '–', '_', '.', '  '];
		for (let i = 0; i < 2000; i++) {
			const hex = Array.from({ length: 16 }, () => pick(r, [...'0123456789ABCDEF'])).join('');
			const groups = hex.match(/.{4}/g)!.map((g) => (r() < 0.5 ? g.toLowerCase() : g));
			const sep = pick(r, seps);
			const input = pick(r, ['', ' ', '\t', '"']) + groups.join(sep) + pick(r, ['', '\n', ' ']);
			expect(parseToken(input), input).toBe(hex.match(/.{4}/g)!.join('-'));
		}
	});

	it('property: arbitrary input yields null or a canonical token, never anything else', () => {
		const r = rng(2);
		const alphabet = [...'0123456789abcdefABCDEFxyz -–_.:"/\n\t１Ｆ'];
		for (let i = 0; i < 5000; i++) {
			const input = Array.from({ length: Math.floor(r() * 40) }, () => pick(r, alphabet)).join('');
			const out = parseToken(input);
			if (out !== null) expect(out, input).toMatch(CANONICAL);
		}
	});
});
