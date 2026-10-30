import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StatusPill from './StatusPill.svelte';
import {
	STARTUP_SUPPRESSION_S,
	createProgressSource,
	overlayOf,
	stallReason,
	startProgress,
	type ProgressSnapshot
} from './progress';
import { pillState, type CrawlerSnapshot } from './status';

const get = vi.hoisted(() => vi.fn());
vi.mock('$lib/api/client', () => ({ api: { get } }));

const CLEAR_S = STARTUP_SUPPRESSION_S + 2 * 3600;

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
		'%s: a stall once past the startup suppression, "starting" inside it',
		(reason) => {
			const old = pill(progress({ reasons: [reason], uptimeS: 5 * 3600 + STARTUP_SUPPRESSION_S }));
			expect(old).toMatchObject({ kind: 'stalled', reason });
			for (const uptimeS of [60, 5 * 3600]) {
				expect(pill(progress({ reasons: [reason], uptimeS })).kind).toBe('starting');
			}
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

	it('stalled: nothing stored for a reported window that lies after the startup suppression', () => {
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
