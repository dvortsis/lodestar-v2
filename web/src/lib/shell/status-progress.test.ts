import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StatusPill from './StatusPill.svelte';
import {
	START_GRACE_S,
	createProgressSource,
	overlayOf,
	stallReason,
	startProgress,
	type ProgressSnapshot
} from './progress';
import { pillState, type CrawlerSnapshot } from './status';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', async (actual) => ({
	...(await actual<typeof import('$lib/api/client')>()),
	api: { get }
}));

const CLEAR_S = START_GRACE_S + 2 * 3600;

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

	it.each(['writer_stalled', 'cohort_stalled'] as const)(
		'%s: a stall once past the start grace; before it, warming up while nothing is stored',
		(reason) => {
			const fresh = crawler({ stored: 0 });
			expect(pill(progress({ reasons: [reason], uptimeS: START_GRACE_S }))).toMatchObject({
				kind: 'stalled',
				reason
			});
			expect(pill(progress({ reasons: [reason], uptimeS: 60 }), fresh).kind).toBe('warming');
			expect(pill(progress({ reasons: [reason], uptimeS: START_GRACE_S - 1 }), fresh).kind).toBe(
				'warming'
			);
			// Something stored since the start: it is simply working.
			expect(pill(progress({ reasons: [reason], uptimeS: 60 })).kind).toBe('crawling');
		}
	);

	it('index_lag is a stall at any uptime', () => {
		for (const uptimeS of [60, 5 * 3600, DAY]) {
			expect(pill(progress({ reasons: ['index_lag'], uptimeS }))).toMatchObject({
				kind: 'stalled',
				reason: 'index_lag'
			});
		}
	});

	it('stalled: nothing stored for a reported window that lies after the start grace', () => {
		const s = pill(progress({ ingestPerHour: 0, fetchedPerHour: 0, uptimeS: CLEAR_S }));
		expect(s).toMatchObject({ kind: 'stalled', reason: 'no_progress' });
	});

	it('fetching that never reaches the store is a stall', () => {
		expect(pill(progress({ ingestPerHour: 0, fetchedPerHour: 300, uptimeS: CLEAR_S })).kind).toBe(
			'stalled'
		);
	});

	it('the reported hour may still lie inside the suppression: not a stall yet', () => {
		const s = pill(progress({ ingestPerHour: 0, fetchedPerHour: 0, uptimeS: CLEAR_S - 1 }));
		expect(s.kind).toBe('crawling');
	});

	it('an idle crawler has no writer to stall, but a starved pipeline still reads as stalled', () => {
		const idle = crawler({ fetch_active: 0, fetch_pending: 0 });
		expect(pill(progress({ reasons: ['writer_stalled'], uptimeS: CLEAR_S }), idle).kind).toBe(
			'idle'
		);
		expect(pill(progress({ ingestPerHour: 0, uptimeS: CLEAR_S }), idle)).toMatchObject({
			kind: 'stalled',
			reason: 'no_progress'
		});
		expect(pill(progress({ reasons: ['index_lag'] }), idle).kind).toBe('stalled');
	});

	it('a starved idle pipeline that also carries writer_stalled reads as no_progress, not idle', () => {
		const idle = crawler({ fetch_active: 0, fetch_pending: 0 });
		const s = pill(
			progress({
				ingestPerHour: 0,
				fetchedPerHour: 0,
				uptimeS: CLEAR_S,
				reasons: ['writer_stalled']
			}),
			idle
		);
		expect(s).toMatchObject({ kind: 'stalled', reason: 'no_progress' });
		// index_lag keeps its own wording even when nothing was stored.
		const lag = pill(
			progress({ ingestPerHour: 0, uptimeS: CLEAR_S, reasons: ['index_lag'] }),
			idle
		);
		expect(lag).toMatchObject({ kind: 'stalled', reason: 'index_lag' });
	});

	it('starting: only while the DHT has no routing nodes, never for hours', () => {
		const joining = crawler({ dht_nodes: 0, stored: 0 });
		const p = progress({ ingestPerHour: 0, fetchedPerHour: 0, uptimeS: 30 });
		expect(pill(p, joining).kind).toBe('starting');
		// Once it has nodes, a quiet start is "warming up", with the live counts.
		expect(pill(p, crawler({ dht_nodes: 3, stored: 0, harvested: 12 }))).toMatchObject({
			kind: 'warming',
			found: 12,
			stored: 0
		});
		// Null nodes (the DHT is off, or hasn't reported) is not "joining".
		expect(pill(p, crawler({ dht_nodes: null, stored: 0 })).kind).toBe('warming');
		// A quiet crawler six hours in is not starting or warming up: the stall rules decide.
		const old = progress({ ingestPerHour: 0, fetchedPerHour: 0, uptimeS: 6 * 3600 });
		expect(pill(old, crawler({ stored: 0 }))).toMatchObject({
			kind: 'stalled',
			reason: 'no_progress'
		});
	});

	it('warming up ends at the first stored torrent', () => {
		const p = progress({ uptimeS: 120, ingestPerHour: null, fetchedPerHour: null });
		expect(pill(p, crawler({ stored: 0 })).kind).toBe('warming');
		expect(pill(p, crawler({ stored: 1 })).kind).toBe('crawling');
	});

	it('starting does not hide a healthy young crawler', () => {
		expect(pill(progress({ uptimeS: 600 })).kind).toBe('crawling');
	});

	describe('the server says whether sampling is held back (sampling_blocked)', () => {
		const quiet = { ingestPerHour: 0, fetchedPerHour: 0 };
		const held = (reason: string, remaining_s = 600) => ({ sampling: { reason, remaining_s } });

		it('held back: starting, whatever the uptime says', () => {
			for (const uptimeS of [30, DAY]) {
				const s = pill(progress({ ...quiet, uptimeS, ...held('dirty') }));
				expect(s.kind).toBe('starting');
				expect(s.reason).toBeUndefined();
			}
			// Its quiet reasons are what holding back looks like, not a stall.
			const s = pill(progress({ ...held('off'), reasons: ['writer_stalled'], uptimeS: DAY }));
			expect(s.kind).toBe('starting');
		});

		it('every reason (the start hold, the crash-loop guard) reads as plain "Starting"', () => {
			for (const reason of ['locked', 'dirty', 'invalidate_failed']) {
				const s = pill(progress({ ...quiet, uptimeS: 2 * DAY, ...held(reason, 10) }));
				expect(s.kind).toBe('starting');
				expect(s.reason).toBeUndefined();
			}
		});

		it('sampling at once (a resumed restart): quiet reasons count after the start grace', () => {
			const young = pill(
				progress({ reasons: ['writer_stalled'], uptimeS: START_GRACE_S - 1, sampling: null })
			);
			expect(young.kind).toBe('crawling');
			const s = pill(
				progress({ reasons: ['writer_stalled'], uptimeS: START_GRACE_S, sampling: null })
			);
			expect(s).toMatchObject({ kind: 'stalled', reason: 'writer_stalled' });
			// index_lag is never a fresh start.
			const lag = pill(progress({ reasons: ['index_lag'], uptimeS: 60, sampling: null }));
			expect(lag).toMatchObject({ kind: 'stalled', reason: 'index_lag' });
		});

		it('after a suppression, quiet reasons wait their own window from when sampling began', () => {
			const at = 30 * 3600;
			const base = { sampling: null, sampledFromS: at };
			const s = (reason: string, uptimeS: number) =>
				pill(progress({ ...base, reasons: [reason], uptimeS }));
			expect(s('writer_stalled', at + START_GRACE_S - 1).kind).toBe('crawling');
			expect(s('writer_stalled', at + START_GRACE_S).kind).toBe('stalled');
			expect(s('cohort_stalled', at + 2 * 3600 - 1).kind).toBe('crawling');
			expect(s('cohort_stalled', at + 2 * 3600)).toMatchObject({
				kind: 'stalled',
				reason: 'cohort_stalled'
			});
		});

		it('a stored rate of 0 counts only once its hour lies after sampling was held back', () => {
			const at = 30 * 3600;
			const base = { ...quiet, sampling: null, sampledFromS: at };
			expect(pill(progress({ ...base, uptimeS: at + 2 * 3600 - 1 })).kind).toBe('crawling');
			expect(pill(progress({ ...base, uptimeS: at + 2 * 3600 }))).toMatchObject({
				kind: 'stalled',
				reason: 'no_progress'
			});
		});

		it('absent or null: sampling is not blocked, so never "Starting" from the clock', () => {
			for (const sampling of [undefined, null]) {
				for (const uptimeS of [30, 600, 6 * 3600]) {
					const s = pill(progress({ ...quiet, uptimeS, sampling }));
					expect(s.kind).not.toBe('starting');
				}
			}
			expect(overlayOf(progress({ uptimeS: DAY, sampling: undefined }))).toBeNull();
		});
	});

	it('paused wins over a zero rate', () => {
		const s = pill(progress({ ingestPerHour: 0, fetchedPerHour: 0 }), crawler({ paused: true }));
		expect(s.kind).toBe('paused');
	});

	it('offline wins over everything', () => {
		const s = pill(progress({ reasons: ['writer_stalled'] }), crawler(), 'offline' as 'live');
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
		expect(screen.getByTestId('status-pill-text').textContent).toBe('Needs attention');
		expect(link.querySelector('svg[aria-hidden="true"]')).toBeTruthy();
		expect(screen.getByTestId('status-pill-announce').textContent).toContain('Open Insights');
	});

	it('starting: says sampling begins once the DHT has joined, with no wait named', () => {
		render(StatusPill, { state: { kind: 'starting', ...none } });
		expect(screen.getByTestId('status-pill-text').textContent).toBe('Starting');
		const help = screen.getByTestId('status-pill-announce').textContent;
		expect(help).toContain('as soon as Lodestar has joined the DHT');
		expect(help).not.toMatch(/\d\s*h\b/);
	});

	it('warming up: live counts found and stored, and no promise of speed', () => {
		render(StatusPill, { state: { kind: 'warming', ...none, found: 1200, stored: 0 } });
		expect(screen.getByTestId('status-pill-text').textContent).toBe('Warming up');
		expect(screen.getByTestId('status-pill-count').textContent).toContain('1.2K found · 0 stored');
		const help = screen.getByTestId('status-pill-announce').textContent;
		expect(help).toContain('days and weeks');
		expect(screen.getByTestId('status-pill').querySelector('svg[aria-hidden="true"]')).toBeTruthy();
	});

	it('warming up with unknown counts shows "—", never 0', () => {
		render(StatusPill, { state: { kind: 'warming', ...none, found: null, stored: null } });
		expect(screen.getByTestId('status-pill-count').textContent).toContain('— found · — stored');
	});

	it('the other states announce nothing extra', () => {
		render(StatusPill, { state: { kind: 'crawling', ...none } });
		expect(screen.getByTestId('status-pill-announce').textContent).toBe('');
	});
});

describe('StatusPill other states', () => {
	const none = { torrents: 5, done: null, total: null };
	it.each([
		['paused', 'Paused'],
		['reconnecting', 'Reconnecting']
	] as const)('%s reads as words and announces nothing extra', (kind, words) => {
		render(StatusPill, { state: { kind, ...none } });
		expect(screen.getByTestId('status-pill-text').textContent).toBe(words);
		expect(screen.getByTestId('status-pill-announce').textContent).toBe('');
	});

	it('an index lag explains itself as lag, not as nothing arriving', () => {
		render(StatusPill, { state: { kind: 'stalled', ...none, reason: 'index_lag' } });
		expect(screen.getByTestId('status-pill-announce').textContent).toContain('index is behind');
	});
});

describe('progress source', () => {
	afterEach(() => {
		get.mockReset();
		vi.useRealTimers();
	});
	const stats = (over = {}) => ({
		fetched_per_hour: 1200,
		index_lag_s: 1,
		paused: false,
		uptime_s: DAY,
		...over
	});
	const overview = (over = {}) => ({
		ingest_per_hour: 900,
		kpi: { health: { reason: null } },
		stale: null,
		exceptions: [],
		...over
	});
	const route = (s: unknown, o: unknown) =>
		get.mockImplementation((path: string) =>
			path === '/stats'
				? s instanceof Error
					? Promise.reject(s)
					: Promise.resolve(s)
				: o instanceof Error
					? Promise.reject(o)
					: Promise.resolve(o)
		);
	const overviewCalls = () => get.mock.calls.filter((c) => c[0] === '/insights/overview').length;

	it('does not ask the overview while fetching flows and the index is current', async () => {
		route(stats(), overview());
		const p = await createProgressSource()();
		expect(overviewCalls()).toBe(0);
		expect(p).toMatchObject({ fetchedPerHour: 1200, ingestPerHour: null, uptimeS: DAY });
	});

	it('passes sampling_blocked through, and leaves it unknown without /stats', async () => {
		const blocked = { reason: 'dirty', remaining_s: 600 };
		route(stats({ sampling_blocked: blocked }), overview());
		expect(await createProgressSource()()).toMatchObject({ sampling: blocked });
		route(stats({ sampling_blocked: null }), overview());
		expect(await createProgressSource()()).toMatchObject({ sampling: null });
		route(new Error('down'), overview());
		expect((await createProgressSource()())?.sampling).toBeUndefined();
	});

	it('remembers the uptime at which sampling stopped being held back, until a restart', async () => {
		const source = createProgressSource();
		const blocked = { reason: 'locked', remaining_s: 60 };
		route(stats({ uptime_s: 100, sampling_blocked: blocked }), overview());
		expect(await source()).toMatchObject({ sampledFromS: null });
		route(stats({ uptime_s: 200, sampling_blocked: null }), overview());
		expect(await source()).toMatchObject({ sampledFromS: 200 });
		route(stats({ uptime_s: 300, sampling_blocked: null }), overview());
		expect(await source()).toMatchObject({ sampledFromS: 200 });
		// A restart starts over.
		route(stats({ uptime_s: 10, sampling_blocked: null }), overview());
		expect(await source()).toMatchObject({ sampledFromS: null });
	});

	it('asks at once when fetching is 0 or the index lags, and maps the reasons', async () => {
		route(
			stats({ fetched_per_hour: 0 }),
			overview({
				ingest_per_hour: 0,
				kpi: { health: { reason: 'writer_stalled' } },
				exceptions: [{ kind: 'cohort_stalled' }]
			})
		);
		const p = await createProgressSource()();
		expect(overviewCalls()).toBe(1);
		expect(p).toMatchObject({ ingestPerHour: 0, reasons: ['writer_stalled', 'cohort_stalled'] });
		get.mockClear();
		route(stats({ index_lag_s: 99 }), overview());
		await createProgressSource()();
		expect(overviewCalls()).toBe(1);
	});

	it('does not treat an unmeasured (null) rate as odd, and skips the overview while paused', async () => {
		route(stats({ fetched_per_hour: null }), overview());
		await createProgressSource()();
		route(stats({ fetched_per_hour: 0, paused: true }), overview());
		await createProgressSource()();
		expect(overviewCalls()).toBe(0);
	});

	it('asks the overview every few minutes even when /stats looks fine, and keeps its facts between', async () => {
		route(stats(), overview({ ingest_per_hour: 0 }));
		const source = createProgressSource();
		const t0 = Date.now();
		await source(t0);
		expect(overviewCalls()).toBe(0);
		const due = await source(t0 + 5 * 60_000);
		expect(overviewCalls()).toBe(1);
		expect(due?.ingestPerHour).toBe(0);
		const between = await source(t0 + 6 * 60_000);
		expect(overviewCalls()).toBe(1);
		expect(between?.ingestPerHour).toBe(0);
	});

	it('drops the kept stored-rate facts when uptime goes down (a restart)', async () => {
		const source = createProgressSource();
		const t0 = Date.now();
		route(
			stats({ uptime_s: DAY }),
			overview({ ingest_per_hour: 0, stale: { reason: 'writer_stalled' } })
		);
		await source(t0 + 5 * 60_000);
		const kept = await source(t0 + 6 * 60_000);
		expect(kept?.reasons).toEqual(['writer_stalled']);
		route(stats({ uptime_s: 30 }), overview());
		const after = await source(t0 + 7 * 60_000);
		expect(after).toMatchObject({ ingestPerHour: null, reasons: [] });
	});

	it('a failed request leaves its facts unknown; both failing gives null', async () => {
		route(new Error('down'), overview({ ingest_per_hour: 0 }));
		const source = createProgressSource();
		const p = await source(Date.now() + 10 * 60_000);
		expect(p).toMatchObject({ fetchedPerHour: null, uptimeS: null, ingestPerHour: 0 });
		route(new Error('down'), new Error('down'));
		expect(await createProgressSource()(Date.now() + 10 * 60_000)).toBeNull();
	});

	it('polls every minute, skips a hidden tab, keeps the last overlay on null, and stops', async () => {
		vi.useFakeTimers();
		const set = vi.fn();
		const answers: (ProgressSnapshot | null)[] = [
			progress({ reasons: ['writer_stalled'] }),
			null,
			progress()
		];
		const source = vi.fn(() => Promise.resolve(answers.shift() ?? null));
		const stop = startProgress(set, source);
		await vi.advanceTimersByTimeAsync(0);
		expect(set).toHaveBeenCalledWith({ kind: 'stalled', reason: 'writer_stalled' });
		await vi.advanceTimersByTimeAsync(60_000);
		expect(set).toHaveBeenCalledTimes(1); // null: nothing replaced
		Object.defineProperty(document, 'hidden', { value: true, configurable: true });
		await vi.advanceTimersByTimeAsync(60_000);
		expect(source).toHaveBeenCalledTimes(2);
		Object.defineProperty(document, 'hidden', { value: false, configurable: true });
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.advanceTimersByTimeAsync(0);
		expect(set).toHaveBeenLastCalledWith(null);
		expect(source).toHaveBeenCalledTimes(3);
		document.dispatchEvent(new Event('visibilitychange')); // inside the minimum gap: ignored
		await vi.advanceTimersByTimeAsync(0);
		expect(source).toHaveBeenCalledTimes(3);
		stop();
		await vi.advanceTimersByTimeAsync(120_000);
		expect(source).toHaveBeenCalledTimes(3);
	});
});
