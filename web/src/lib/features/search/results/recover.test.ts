import { describe, expect, it } from 'vitest';
import type { components } from '$lib/api/schema';
import { bannerFor, suggestions, type IndexState } from './recover';

type Clause = components['schemas']['ClauseRemoval'];
const clause = (over: Partial<Clause>): Clause => ({
	canonical: 'x',
	clause: 'ext:mkv',
	index: 0,
	total_approx: false,
	without: { op: 'word', field: 'any', word: 'lantern' },
	...over
});

describe('suggestions', () => {
	it('keeps only clauses with a count above 0, in query order', () => {
		const got = suggestions({
			skipped: 2,
			clauses: [
				clause({ clause: 'a', total: 12 }),
				clause({ clause: 'b', total: 0 }),
				clause({ clause: 'c' }),
				clause({ clause: 'd', total: 1_200_000, total_approx: true })
			]
		});
		expect(got.map((g) => [g.clause, g.total, g.approx])).toEqual([
			['a', 12, false],
			['d', 1_200_000, true]
		]);
	});

	it('hands over the server’s reduced query untouched', () => {
		const without = { op: 'word', field: 'any', word: 'only' } as const;
		expect(suggestions({ skipped: 0, clauses: [clause({ total: 3, without })] })[0]!.without).toBe(
			without
		);
	});

	it('is empty without recovery', () => {
		expect(suggestions(undefined)).toEqual([]);
		expect(suggestions(null)).toEqual([]);
	});
});

const state = (over: Partial<IndexState> = {}): IndexState => ({
	cold_start: false,
	growing: false,
	growth_hours: 24,
	growth_per_hour: null,
	rebuild: null,
	searchable_torrents: 5000,
	unavailable: [],
	updated_at: 0,
	...over
});
const rebuild = (phase: 'building' | 'done' | 'failed') => ({
	done: 1,
	total: 2,
	error: null,
	from: 'a',
	to: 'b',
	phase,
	unavailable: ['codec'],
	started_at: 0
});

describe('bannerFor', () => {
	it('is none before the state arrives or for a grown index', () => {
		expect(bannerFor(null, null)).toBeNull();
		expect(bannerFor(state(), null)).toBeNull();
	});

	it('growing carries the rate, and omits a null or zero one', () => {
		expect(bannerFor(state({ growing: true, growth_per_hour: 2104.5 }), null)).toEqual({
			kind: 'growing',
			count: 5000,
			perHour: 2104.5
		});
		for (const rate of [null, 0])
			expect(bannerFor(state({ growing: true, growth_per_hour: rate }), null)).toMatchObject({
				perHour: null
			});
	});

	it('a rebuild in progress wins over growing, and a finished one is gone', () => {
		expect(bannerFor(state({ growing: true, rebuild: rebuild('building') }), null)?.kind).toBe(
			'rebuild'
		);
		expect(bannerFor(state({ growing: true }), rebuild('building'))?.kind).toBe('rebuild');
		expect(bannerFor(state({ rebuild: rebuild('building') }), rebuild('done'))).toBeNull();
		expect(bannerFor(state({ rebuild: rebuild('failed') }), null)).toBeNull();
	});
});
