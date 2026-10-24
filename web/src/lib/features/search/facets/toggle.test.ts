import { describe, expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { brushOfRange, rangeOfBrush, wholeDays, type Bin } from './bins';
import {
	clearField,
	dateOf,
	fieldOf,
	fixedValues,
	owns,
	rangeOf,
	selection,
	setDate,
	setRange,
	toggleValue,
	type FacetField
} from './toggle';

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });
const res = (...any: string[]): Ast => ({ op: 'tag', key: 'res', any });
const ext = (...any: string[]): Ast => ({ op: 'ext', any });
const not = (of: Ast): Ast => ({ op: 'not', of });
const and = (...of: Ast[]): Ast => ({ op: 'and', of });
const or = (...of: Ast[]): Ast => ({ op: 'or', of });
const RES = fieldOf('res')!;
const EXT = fieldOf('ext')!;
const PRIVATE = fieldOf('private')!;

describe('fieldOf', () => {
	it('maps the server facets to query fields', () => {
		expect(fieldOf('type')).toEqual({ kind: 'tag', key: 'type' });
		expect(fieldOf('ext')).toEqual({ kind: 'ext' });
		expect(fieldOf('v2')).toEqual({ kind: 'flag', name: 'v2' });
		expect(fieldOf('category')).toBeNull(); // a share clause: not a rail toggle
		expect(fieldOf('nope')).toBeNull();
	});
});

describe('owns (the server’s own-clause rule)', () => {
	it.each<[string, Ast, FacetField, boolean]>([
		['a leaf', res('1080p'), RES, true],
		['an OR of leaves', or(res('1080p'), res('720p')), RES, true],
		['a negation', not(res('480p')), RES, true],
		['another field', ext('mkv'), RES, false],
		['a mixed OR', or(res('1080p'), ext('mkv')), RES, false],
		['an empty AND', and(), RES, false],
		['a flag', { op: 'flag', name: 'private', value: true }, PRIVATE, true],
		['the other flag', { op: 'flag', name: 'v2', value: true }, PRIVATE, false]
	])('%s', (_, node, field, want) => expect(owns(node, field)).toBe(want));
});

describe('toggleValue', () => {
	it('adds a clause to an empty query and to free text', () => {
		expect(toggleValue(null, RES, '1080p')).toEqual(res('1080p'));
		expect(toggleValue({ op: 'all' }, RES, '1080p')).toEqual(res('1080p'));
		expect(toggleValue(word('lantern'), RES, '1080p')).toEqual(and(word('lantern'), res('1080p')));
	});

	it('extends the facet’s own clause in place, and drops it with its last value', () => {
		const q = and(word('lantern'), res('1080p'), ext('mkv'));
		const two = toggleValue(q, RES, '2160p');
		expect(two).toEqual(and(word('lantern'), res('1080p', '2160p'), ext('mkv')));
		expect(toggleValue(two, RES, '1080p')).toEqual(and(word('lantern'), res('2160p'), ext('mkv')));
		expect(toggleValue(toggleValue(two, RES, '1080p'), RES, '2160p')).toEqual(
			and(word('lantern'), ext('mkv'))
		);
	});

	it('excludes with a negation, and moves a value between selected and excluded', () => {
		const q = and(word('lantern'), res('1080p'));
		const ex = toggleValue(q, RES, '480p', 'exclude');
		expect(ex).toEqual(and(word('lantern'), res('1080p'), not(res('480p'))));
		expect(toggleValue(ex, RES, '480p')).toEqual(and(word('lantern'), res('1080p', '480p')));
		expect(toggleValue(ex, RES, '1080p', 'exclude')).toEqual(
			and(word('lantern'), not(res('480p', '1080p')))
		);
		expect(toggleValue(ex, RES, '480p', 'exclude')).toEqual(and(word('lantern'), res('1080p')));
	});

	it('rewrites only the first positive own clause: a second one is an AND, not an OR', () => {
		// (1080p | 720p) AND 2160p: dropping 720p leaves 1080p AND 2160p, never 1080p OR 2160p.
		const q = and(or(res('1080p'), res('720p')), word('x'), res('2160p'));
		expect(selection(q, RES)).toEqual({ include: ['1080p', '720p', '2160p'], exclude: [] });
		expect(toggleValue(q, RES, '720p')).toEqual(and(res('1080p'), word('x'), res('2160p')));
		expect(fixedValues(q, RES)).toEqual(new Set(['2160p']));
		// lang:en lang:fr (both) plus de: en or de, and still fr.
		const both = and(res('en'), res('fr'));
		expect(toggleValue(both, RES, 'de')).toEqual(and(res('en', 'de'), res('fr')));
	});

	it('merges negations: -a -b is -(a|b)', () => {
		const q = and(not(res('480p')), word('x'), not(res('576p')));
		expect(toggleValue(q, RES, '720p', 'exclude')).toEqual(
			and(not(res('480p', '576p', '720p')), word('x'))
		);
		expect(fixedValues(q, RES)).toEqual(new Set());
	});

	it('names the values only the query text can change', () => {
		expect(fixedValues(and(word('x'), not(and(res('a'), res('b')))), RES)).toEqual(
			new Set(['a', 'b'])
		);
		expect(fixedValues(or(res('a'), not(res('b'))), RES)).toEqual(new Set(['a', 'b']));
		expect(fixedValues(and(res('a'), not(res('b'))), RES)).toEqual(new Set());
		expect(fixedValues(or(res('a'), ext('mkv')), RES)).toEqual(new Set()); // not the facet's
	});

	it('leaves mixed clauses alone: they belong to no facet', () => {
		const mixed = or(res('1080p'), ext('mkv'));
		const q = and(mixed, word('x'));
		expect(selection(q, RES)).toEqual({ include: [], exclude: [] });
		expect(toggleValue(q, RES, '720p')).toEqual(and(mixed, word('x'), res('720p')));
	});

	it('reads double negations as the server does', () => {
		expect(selection(not(not(res('1080p'))), RES)).toEqual({ include: ['1080p'], exclude: [] });
		expect(selection(not(or(res('480p'), res('576p'))), RES)).toEqual({
			include: [],
			exclude: ['480p', '576p']
		});
	});

	it('keeps one flag value: selecting, switching and clearing', () => {
		const yes: Ast = { op: 'flag', name: 'private', value: true };
		const no: Ast = { op: 'flag', name: 'private', value: false };
		expect(toggleValue(word('x'), PRIVATE, 'yes')).toEqual(and(word('x'), yes));
		expect(toggleValue(and(word('x'), yes), PRIVATE, 'no')).toEqual(and(word('x'), no));
		expect(toggleValue(and(word('x'), yes), PRIVATE, 'yes')).toEqual(word('x'));
		expect(toggleValue(word('x'), PRIVATE, 'yes', 'exclude')).toEqual(and(word('x'), no));
	});

	it('clears a facet', () => {
		expect(clearField(and(word('x'), res('1080p'), not(res('480p'))), RES)).toEqual(word('x'));
		expect(clearField(res('1080p'), RES)).toEqual({ op: 'all' });
	});

	it('works on ext', () => {
		expect(toggleValue(ext('mkv'), EXT, 'mp4')).toEqual(ext('mkv', 'mp4'));
	});
});

describe('ranges and dates', () => {
	it('replaces the field’s range in place, and removes it', () => {
		const q = and(word('x'), { op: 'range', field: 'size', lo: 1, hi: 2 }, ext('mkv'));
		const next = setRange(q, 'size', 10, null);
		expect(next).toEqual(
			and(word('x'), { op: 'range', field: 'size', lo: 10, hi: null }, ext('mkv'))
		);
		expect(rangeOf(next, 'size')).toEqual({ lo: 10, hi: null });
		expect(rangeOf(next, 'seeders')).toBeNull();
		expect(setRange(next, 'size', null, null)).toEqual(and(word('x'), ext('mkv')));
		expect(setRange(word('x'), 'seeders', 5, null)).toEqual(
			and(word('x'), { op: 'range', field: 'seeders', lo: 5, hi: null })
		);
	});

	it('replaces the date clause', () => {
		const week = setDate(word('x'), 'discovered', { ago: 7 * 86_400 }, null);
		expect(week).toEqual(
			and(word('x'), { op: 'date', field: 'discovered', from: { ago: 604_800 }, to: null })
		);
		expect(dateOf(week, 'discovered')).toEqual({ from: { ago: 604_800 }, to: null });
		const day = setDate(week, 'discovered', { ago: 86_400 }, null);
		expect(dateOf(day, 'discovered')).toEqual({ from: { ago: 86_400 }, to: null });
		expect(setDate(day, 'discovered', null, null)).toEqual(word('x'));
	});
});

describe('bins', () => {
	const bins: Bin[] = [
		{ lo: 0, hi: 100, count: 1, backfill: false },
		{ lo: 100, hi: 200, count: 2, backfill: false },
		{ lo: 200, hi: 400, count: 3, backfill: false },
		{ lo: 400, hi: null, count: 4, backfill: false }
	];
	it('maps a brush to an inclusive range, open at either end of the histogram', () => {
		expect(rangeOfBrush(bins, { from: 1, to: 3 })).toEqual({ lo: 100, hi: 399 });
		expect(rangeOfBrush(bins, { from: 0, to: 2 })).toEqual({ lo: null, hi: 199 });
		expect(rangeOfBrush(bins, { from: 2, to: 4 })).toEqual({ lo: 200, hi: null });
		expect(rangeOfBrush(bins, { from: 0, to: 4 })).toBeNull();
	});
	it('maps a range back to the bins it overlaps', () => {
		expect(brushOfRange(bins, { lo: 100, hi: 399 })).toEqual({ from: 1, to: 3 });
		expect(brushOfRange(bins, { lo: 150, hi: null })).toEqual({ from: 1, to: 4 });
		expect(brushOfRange(bins, { lo: null, hi: 50 })).toEqual({ from: 0, to: 1 });
		expect(brushOfRange(bins, null)).toBeNull();
		expect(brushOfRange([], { lo: 1, hi: 2 })).toBeNull();
	});
	it('round-trips every brush', () => {
		for (let from = 0; from < bins.length; from++)
			for (let to = from + 1; to <= bins.length; to++) {
				const r = rangeOfBrush(bins, { from, to });
				if (r) expect(brushOfRange(bins, r)).toEqual({ from, to });
			}
	});
});

describe('toggleValue, every state of three values', () => {
	// Each value is unset, selected or excluded: 27 starting queries, each with free text around
	// the facet's clauses, times every toggle (3 values × include/exclude).
	const VALUES = ['480p', '1080p', '2160p'] as const;
	type State = 'none' | 'in' | 'out';
	const states: State[][] = [];
	for (let n = 0; n < 27; n++)
		states.push(
			VALUES.map((_, i) => (['none', 'in', 'out'] as const)[Math.floor(n / 3 ** i) % 3]!)
		);
	const build = (s: State[]): Ast => {
		const inc = VALUES.filter((_, i) => s[i] === 'in');
		const exc = VALUES.filter((_, i) => s[i] === 'out');
		const of: Ast[] = [word('lantern')];
		if (inc.length) of.push(res(...inc));
		of.push(word('keeper'));
		if (exc.length) of.push(not(res(...exc)));
		return and(...of);
	};
	it.each(states.map((s) => [s.join(','), s] as const))('from %s', (_, s) => {
		const q = build(s);
		VALUES.forEach((value, i) => {
			for (const how of ['include', 'exclude'] as const) {
				const target: State = how === 'include' ? 'in' : 'out';
				const want = s.map((st, j) => (j !== i ? st : st === target ? 'none' : target));
				const next = toggleValue(q, RES, value, how);
				const sel = selection(next, RES);
				expect(new Set(sel.include)).toEqual(new Set(VALUES.filter((_, j) => want[j] === 'in')));
				expect(new Set(sel.exclude)).toEqual(new Set(VALUES.filter((_, j) => want[j] === 'out')));
				// The free text stays, in order, and the facet has at most two clauses.
				const tops = next.op === 'and' ? next.of : [next];
				expect(
					tops.filter((n) => n.op === 'word').map((n) => (n as { word: string }).word)
				).toEqual(['lantern', 'keeper']);
				expect(tops.filter((n) => owns(n, RES)).length).toBeLessThanOrEqual(2);
			}
		});
	});
});

describe('toggleValue over random queries (seeded)', () => {
	// A small LCG, so a failure names a seed that reproduces it.
	function rng(seed: number) {
		let x = seed >>> 0 || 1;
		return () => (x = (Math.imul(x, 1_664_525) + 1_013_904_223) >>> 0) / 2 ** 32;
	}
	const VALUES = ['480p', '720p', '1080p', '2160p'];
	function generate(r: () => number): Ast {
		const pick = <T>(xs: T[]) => xs[Math.floor(r() * xs.length)]!;
		const anyRes = (): Ast => res(...VALUES.filter(() => r() < 0.4).concat(pick(VALUES)));
		const shapes: (() => Ast)[] = [
			() => word(pick(['lantern', 'keeper', 'tide'])),
			() => ext(pick(['mkv', 'mp4'])),
			anyRes,
			() => not(anyRes()),
			() => or(anyRes(), anyRes()),
			() => not(or(anyRes(), anyRes())),
			() => and(anyRes(), anyRes()), // not rewritable: (a AND b)
			() => not(and(anyRes(), anyRes())), // not rewritable: NOT (a AND b)
			() => or(anyRes(), ext('mkv')), // mixed: not the facet's
			() => or(anyRes(), not(anyRes())) // own, but not rewritable
		];
		const of = Array.from({ length: 1 + Math.floor(r() * 5) }, () => pick(shapes)());
		return of.length === 1 ? of[0]! : and(...of);
	}
	const tops = (a: Ast) => (a.op === 'and' ? a.of : a.op === 'all' ? [] : [a]);
	const plain = (n: Ast) =>
		n.op === 'tag' || (n.op === 'or' && n.of.every((c) => c.op === 'tag' && c.key === 'res'));
	// What a toggle may rewrite: the first positive plain own clause, and every negated one.
	const rewrittenOf = (a: Ast) => {
		const first = tops(a).find((n) => owns(n, RES) && plain(n));
		return tops(a).filter((n) => n === first || (owns(n, RES) && n.op === 'not' && plain(n.of)));
	};

	// The query as a predicate over a torrent: its res values and its extensions (free text holds).
	type Doc = { res: Set<string>; ext: Set<string> };
	const holds = (n: Ast, d: Doc): boolean => {
		if (n.op === 'and') return n.of.every((c) => holds(c, d));
		if (n.op === 'or') return n.of.some((c) => holds(c, d));
		if (n.op === 'not') return !holds(n.of, d);
		if (n.op === 'tag') return n.any.some((v) => d.res.has(v));
		if (n.op === 'ext') return n.any.some((v) => d.ext.has(v));
		return true;
	};
	const subsets = <T>(xs: T[]): T[][] =>
		xs.reduce<T[][]>((acc, x) => acc.concat(acc.map((s) => [...s, x])), [[]]);
	const DOCS: Doc[] = subsets(VALUES).flatMap((r) =>
		subsets(['mkv', 'mp4']).map((e) => ({ res: new Set(r), ext: new Set(e) }))
	);

	for (let seed = 1; seed <= 300; seed++) {
		it(`seed ${seed}`, () => {
			const r = rng(seed);
			const q = generate(r);
			const value = VALUES[Math.floor(r() * VALUES.length)]!;
			const how = r() < 0.5 ? 'include' : 'exclude';
			const next = toggleValue(q, RES, value, how);
			// Everything the toggle may not rewrite is still there, in order.
			const kept = (a: Ast) => tops(a).filter((n) => !rewrittenOf(a).includes(n));
			expect(kept(next)).toEqual(kept(q));
			// At most two rewritten clauses remain, and the value is where the toggle put it.
			const own = rewrittenOf(next);
			expect(own.length).toBeLessThanOrEqual(2);
			const sel = { include: [] as string[], exclude: [] as string[] };
			for (const n of own) {
				const s = selection(n, RES);
				sel.include.push(...s.include);
				sel.exclude.push(...s.exclude);
			}
			const before = { include: [] as string[], exclude: [] as string[] };
			for (const n of rewrittenOf(q)) {
				const s = selection(n, RES);
				before.include.push(...s.include);
				before.exclude.push(...s.exclude);
			}
			// The meaning: only torrents with the clicked value can change. Except when the selected
			// list starts or ends (from none to one value, or back): adding or dropping that clause is
			// what selecting the first value, or un-selecting the last, means.
			const flipped = (before.include.length === 0) !== (sel.include.length === 0);
			if (!flipped)
				for (const d of DOCS.filter((d) => !d.res.has(value)))
					expect(holds(next, d), JSON.stringify([...d.res, ...d.ext])).toBe(holds(q, d));
			const was = how === 'include' ? before.include : before.exclude;
			const now = how === 'include' ? sel.include : sel.exclude;
			const other = how === 'include' ? sel.exclude : sel.include;
			expect(now.includes(value)).toBe(!was.includes(value));
			expect(other.includes(value)).toBe(false);
			// Toggling it back restores that list (as a set), unless it came from the other one.
			const otherBefore = how === 'include' ? before.exclude : before.include;
			if (!otherBefore.includes(value)) {
				const back = toggleValue(next, RES, value, how);
				const again = new Set<string>();
				for (const n of rewrittenOf(back)) {
					const s = selection(n, RES);
					(how === 'include' ? s.include : s.exclude).forEach((v) => again.add(v));
				}
				expect(again).toEqual(new Set(was));
			}
		});
	}
});

describe('brushes on bins narrowed to the clause (Discovered)', () => {
	// The clause is Jan 1–Jan 10 (day seconds); the server's bins span only those matches.
	const D = 86_400;
	const narrowed: Bin[] = Array.from({ length: 10 }, (_, i) => ({
		lo: i * D,
		hi: (i + 1) * D,
		count: 5,
		backfill: false
	}));
	const clause = { lo: 0, hi: 10 * D - 1 };
	it('moving one edge keeps the other bound', () => {
		const brush = brushOfRange(narrowed, clause)!;
		expect(brush).toEqual({ from: 0, to: 10 });
		expect(rangeOfBrush(narrowed, { from: 1, to: 10 }, clause)).toEqual({ lo: D, hi: 10 * D - 1 });
		expect(rangeOfBrush(narrowed, { from: 0, to: 9 }, clause)).toEqual({ lo: 0, hi: 9 * D - 1 });
	});
	it('drags out to an end only where the clause didn’t cut', () => {
		const open = { lo: 3 * D, hi: null };
		expect(rangeOfBrush(narrowed, { from: 0, to: 10 }, open)).toEqual(null);
	});
});

describe('wholeDays', () => {
	it('widens a range to whole UTC days, as the query language prints dates', () => {
		const D = 86_400;
		expect(wholeDays({ lo: 10 * D + 3600, hi: 12 * D + 7200 })).toEqual({
			lo: 10 * D,
			hi: 13 * D - 1
		});
		expect(wholeDays({ lo: null, hi: 5 })).toEqual({ lo: null, hi: D - 1 });
		expect(wholeDays(null)).toBeNull();
	});
});
