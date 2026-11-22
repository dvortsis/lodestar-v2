import { describe, expect, it } from 'vitest';
import { formatCount } from '$lib/format';
import { healthKind, healthSentence, type HealthInput } from './health';
import { overviewFixture } from './fixtures';

const RANGE = 'the last 7 days';
const base = (over: Partial<HealthInput> = {}): HealthInput => ({
	phase: 'ready',
	overview: overviewFixture({ exceptions: [] }),
	startup: 'no',
	stalled: false,
	rangeText: RANGE,
	locale: 'en',
	...over
});
const o = (kpi: object, rest: object = {}) => {
	const f = overviewFixture({ exceptions: [] });
	return overviewFixture({ exceptions: [], ...rest, kpi: { ...f.kpi, ...kpi } });
};

describe('the health sentence says only what the data says', () => {
	it('working: the count is the response field, the period is the selected range', () => {
		const ov = o({ new_searchable: 409 });
		const { kind, text } = healthSentence(base({ overview: ov }));
		expect(kind).toBe('working');
		expect(text).toBe(
			`Lodestar is working. Over ${RANGE}, it made ${formatCount(409)} new torrents searchable. Nothing needs your attention.`
		);
		expect(healthSentence(base({ overview: ov, rangeText: 'all time' })).text).toContain(
			'Over all time'
		);
	});

	it('working: one torrent, and an unknown count, never print 0', () => {
		expect(healthSentence(base({ overview: o({ new_searchable: 1 }) })).text).toContain(
			'1 new torrent searchable'
		);
		const none = healthSentence(base({ overview: o({ new_searchable: null }) })).text;
		expect(none).toContain('is not available');
		expect(none).not.toMatch(/\b0\b/);
	});

	it('counts what needs attention from the exceptions list', () => {
		const two = overviewFixture();
		expect(two.exceptions).toHaveLength(2);
		const text = healthSentence(base({ overview: two })).text;
		expect(text).toContain('2 things need your attention.');
		expect(text).not.toContain('Nothing needs');
		const one = overviewFixture({ exceptions: [two.exceptions[0]!] });
		expect(healthSentence(base({ overview: one })).text).toContain('1 thing needs your attention.');
	});

	it('loading, error, stale and starting never claim it is working', () => {
		const cases: [Partial<HealthInput>, string][] = [
			[{ phase: 'loading', overview: undefined }, 'loading'],
			[{ phase: 'error', overview: undefined }, 'error'],
			[{ overview: o({}, { stale: { reason: 'writer_stalled', since: 1 } }) }, 'stale'],
			[{ startup: 'yes' }, 'starting']
		];
		for (const [over, kind] of cases) {
			const r = healthSentence(base(over));
			expect(r.kind).toBe(kind);
			expect(r.text).not.toMatch(/is working|Nothing needs/);
		}
	});

	it('paused, stalled and degraded each have their own line', () => {
		expect(
			healthSentence(base({ overview: o({ health: { state: 'paused', reason: null } }) }))
		).toMatchObject({ kind: 'paused', text: expect.stringContaining('paused') });
		const stalled = healthSentence(base({ stalled: true }));
		expect(stalled.kind).toBe('stalled');
		expect(stalled.text).not.toContain('is working');
		const degraded = healthSentence(
			base({ overview: o({ health: { state: 'degraded', reason: 'index_lag' } }) })
		);
		expect(degraded.kind).toBe('degraded');
		expect(degraded.text).not.toContain('is working');
	});

	it('picks the kind in order: error, stale, paused, starting, stalled, degraded', () => {
		const paused = o({ health: { state: 'paused', reason: null } });
		expect(healthKind(base({ overview: paused, startup: 'yes', stalled: true }))).toBe('paused');
		expect(healthKind(base({ startup: 'yes', stalled: true }))).toBe('starting');
	});
});
