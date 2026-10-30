import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StatusPill from './StatusPill.svelte';
import { STARTUP_SUPPRESSION_S, overlayOf, stallReason, type ProgressSnapshot } from './progress';
import { pillState, type CrawlerSnapshot } from './status';

const crawler = (over: Partial<CrawlerSnapshot> = {}): CrawlerSnapshot => ({
	at: 1,
	dht_nodes: 100,
	fetch_active: 4,
	fetch_pending: 10,
	fetched: 50,
	harvested: 500,
	paused: false,
	stored: 40,
	torrents: 1000,
	...over
});
const DAY = 86_400;
const progress = (over: Partial<ProgressSnapshot> = {}): ProgressSnapshot => ({
	fetchedPerHour: 1260,
	ingestPerHour: 900,
	uptimeS: DAY,
	reasons: [],
	...over
});
const pill = (
	p: ProgressSnapshot | null,
	c: CrawlerSnapshot = crawler(),
	stream = 'live' as const
) => pillState({ stream, crawler: c, rebuild: null, progress: p && overlayOf(p) });

describe('pill progress states', () => {
	it('healthy: rates flowing is crawling', () => {
		expect(pill(progress()).kind).toBe('crawling');
	});

	it('healthy without progress facts yet is unchanged (no stall is claimed from unknowns)', () => {
		expect(pill(null).kind).toBe('crawling');
		expect(pill(progress({ ingestPerHour: null, fetchedPerHour: null })).kind).toBe('crawling');
	});

	it('stalled: a named reason from the API, at any uptime', () => {
		for (const reason of ['writer_stalled', 'cohort_stalled', 'index_lag'] as const) {
			const s = pill(progress({ reasons: [reason], uptimeS: 60 }));
			expect(s.kind).toBe('stalled');
			expect(s.reason).toBe(reason);
		}
	});

	it('stalled: nothing stored for the reported window once the startup suppression is over', () => {
		const s = pill(
			progress({ ingestPerHour: 0, fetchedPerHour: 0, uptimeS: STARTUP_SUPPRESSION_S })
		);
		expect(s).toMatchObject({ kind: 'stalled', reason: 'no_progress' });
	});

	it('a zero stored hour while fetches are flowing is recovering, not stalled', () => {
		expect(pill(progress({ ingestPerHour: 0, fetchedPerHour: 300 })).kind).toBe('crawling');
	});

	it('starting: under six hours up with nothing flowing yet', () => {
		const s = pill(
			progress({ ingestPerHour: 0, fetchedPerHour: 0, uptimeS: STARTUP_SUPPRESSION_S - 1 })
		);
		expect(s.kind).toBe('starting');
		expect(pill(progress({ ingestPerHour: null, fetchedPerHour: null, uptimeS: 30 })).kind).toBe(
			'starting'
		);
	});

	it('starting does not hide a healthy young crawler', () => {
		expect(pill(progress({ uptimeS: 600 })).kind).toBe('crawling');
	});

	it('paused wins over a zero rate', () => {
		const s = pill(progress({ ingestPerHour: 0, fetchedPerHour: 0 }), crawler({ paused: true }));
		expect(s.kind).toBe('paused');
	});

	it('offline wins over everything', () => {
		const s = pill(progress({ reasons: ['writer_stalled'] }), crawler(), 'offline' as never);
		expect(s.kind).toBe('reconnecting');
	});

	it('stallReason ignores reasons that are not stalls', () => {
		expect(stallReason(progress({ reasons: ['paused', 'something_else'] }))).toBeNull();
	});
});

describe('StatusPill progress states', () => {
	const none = { torrents: 1000, done: null, total: null };

	it('stalled: warning tone, words, and an announcement that explains it', () => {
		render(StatusPill, { state: { kind: 'stalled', ...none, reason: 'writer_stalled' } });
		const link = screen.getByTestId('status-pill');
		expect(link.getAttribute('data-state')).toBe('stalled');
		expect(link.getAttribute('href')).toBe('/insights');
		expect(link.className).toContain('text-warning');
		expect(screen.getByTestId('status-pill-text').textContent).toBe('Stalled');
		expect(link.querySelector('svg[aria-hidden="true"]')).toBeTruthy();
		expect(screen.getByTestId('status-pill-announce').textContent).toContain('Open Insights');
	});

	it('starting: says when sampling resumes', () => {
		render(StatusPill, { state: { kind: 'starting', ...none } });
		expect(screen.getByTestId('status-pill-text').textContent).toBe('Starting');
		expect(screen.getByTestId('status-pill-announce').textContent).toContain(
			'sampling resumes after up to 6 h'
		);
	});

	it('the other states announce nothing extra', () => {
		render(StatusPill, { state: { kind: 'crawling', ...none } });
		expect(screen.getByTestId('status-pill-announce').textContent).toBe('');
	});
});
