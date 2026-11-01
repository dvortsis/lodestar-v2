// The overview's data: one request per state change through a `latest()` slot (§10.12), the
// crawler controls (every action refetches, never updates a number by hand), and the live feed.
import { untrack } from 'svelte';
import { api, ApiError, isAbort, latest } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { events } from '$lib/state/events.svelte';
import { apiQuery, type InsightsQuery } from './query';
import { startupState, type Overview, type Startup } from './model';

export type CrawlerStatus = components['schemas']['CrawlerStatus'];
export type CrawlerEvent = components['schemas']['CrawlerEvent'];
export type Intensity = CrawlerStatus['intensity'];
export type Load = 'loading' | 'ready' | 'error';
export type Notice = 'paused' | 'resumed';
export type CrawlerAction = 'pause' | 'resume' | 'intensity';

export function createInsights() {
	let overview = $state.raw<Overview>();
	let phase = $state<Load>('loading');
	let error = $state.raw<ApiError>();
	let all = $state.raw<Overview>();
	let allFailed = $state(false);
	let crawler = $state.raw<CrawlerStatus>();
	let live = $state.raw<CrawlerEvent>();
	let busy = $state<CrawlerAction>();
	let crawlerError = $state.raw<{ action: CrawlerAction; message: string }>();
	let notice = $state<Notice>();

	let query: InsightsQuery = { range: '7d', compare: false };
	const overviewSlot = latest();
	const allSlot = latest();
	const crawlerSlot = latest();

	async function checkAll(q: InsightsQuery, current: Overview) {
		if (q.range === 'all' || current.funnel_total.final_cohorts > 0) return;
		try {
			const r = await allSlot.run((signal) =>
				api.get('/insights/overview', { query: { range: 'all' }, signal })
			);
			all = r;
			allFailed = false;
		} catch (e) {
			// Unknown is not "starting up": the page then just shows the data it has.
			if (!isAbort(e)) allFailed = true;
		}
	}

	/** `quiet` keeps what is on screen and swaps the numbers in place (a live refetch). */
	async function fetchOverview(q: InsightsQuery, quiet: boolean) {
		query = q;
		if (!quiet) {
			overview = undefined;
			all = undefined;
			allFailed = false;
			error = undefined;
			phase = 'loading';
		}
		try {
			const r = await overviewSlot.run((signal) =>
				api.get('/insights/overview', { query: apiQuery(q), signal })
			);
			overview = r;
			error = undefined;
			phase = 'ready';
			await checkAll(q, r);
		} catch (e) {
			if (isAbort(e)) return;
			if (quiet && overview) return;
			error = e instanceof ApiError ? e : undefined;
			phase = 'error';
		}
	}

	async function fetchCrawler() {
		try {
			const before = crawler?.paused;
			const r = await crawlerSlot.run((signal) => api.get('/crawler', { signal }));
			crawler = r;
			if (before !== undefined && before !== r.paused) notice = r.paused ? 'paused' : 'resumed';
		} catch {
			// The controls keep their last state; the next event or action refetches.
		}
	}

	async function act(action: CrawlerAction, run: () => Promise<unknown>) {
		busy = action;
		crawlerError = undefined;
		try {
			await run();
			await Promise.all([fetchCrawler(), fetchOverview(query, true)]);
		} catch (e) {
			if (!isAbort(e)) {
				crawlerError = { action, message: e instanceof ApiError ? e.message : '' };
				// Show what the server really has, not the level that was clicked.
				await fetchCrawler();
			}
		} finally {
			busy = undefined;
		}
	}

	/** Subscribes to the live feed; returns the unsubscribe. */
	function start(): () => void {
		// Called from an effect: reading state here must not make the effect depend on it.
		return untrack(subscribe);
	}

	function subscribe(): () => void {
		void fetchCrawler();
		const offCrawler = events.on(
			'crawler',
			(d) => {
				live = d;
				if (crawler && crawler.paused !== d.paused) {
					void fetchCrawler();
					void fetchOverview(query, true);
				}
			},
			{ onResync: () => void fetchCrawler() }
		);
		// A client showing another range than the default refetches its own on each event.
		const offInsights = events.on('insights', () => void fetchOverview(query, true), {
			onResync: () => void fetchOverview(query, true)
		});
		// The privacy banner's summary follows a Logging or Insights save (S-privacy-banner).
		const offSettings = events.on('settings', (d) => {
			if (d.section === 'logging' || d.section === 'insights') void fetchOverview(query, true);
		});
		return () => {
			offCrawler();
			offInsights();
			offSettings();
			overviewSlot.cancel();
			allSlot.cancel();
			crawlerSlot.cancel();
		};
	}

	return {
		get overview() {
			return overview;
		},
		get phase() {
			return phase;
		},
		get error() {
			return error;
		},
		get crawler() {
			return crawler;
		},
		get live() {
			return live;
		},
		get busy() {
			return busy;
		},
		get crawlerError() {
			return crawlerError;
		},
		get notice() {
			return notice;
		},
		get startup(): Startup {
			if (!overview) return 'no';
			const s = startupState(query.range, overview, all);
			return s === 'check' && (allFailed || !all) ? 'no' : s;
		},
		start,
		/** Loads the overview for `q` (a range or compare change shows the loading state). */
		load(q: InsightsQuery) {
			return fetchOverview(q, false);
		},
		refresh() {
			return fetchOverview(query, true);
		},
		retry() {
			return fetchOverview(query, false);
		},
		pause: () => act('pause', () => api.post('/crawler/pause')),
		resume: () => act('resume', () => api.post('/crawler/resume')),
		setIntensity: (intensity: Intensity) =>
			act('intensity', () => api.patch('/crawler', { body: { intensity } }))
	};
}

export type InsightsState = ReturnType<typeof createInsights>;
