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

	async function act(action: CrawlerAction, run: () => Promise<unknown>): Promise<boolean> {
		busy = action;
		crawlerError = undefined;
		try {
			await run();
			await Promise.all([fetchCrawler(), fetchOverview(query, true)]);
			return true;
		} catch (e) {
			if (!isAbort(e)) {
				crawlerError = { action, message: e instanceof ApiError ? e.message : '' };
				// Show what the server really has, not the level that was clicked.
				await fetchCrawler();
			}
			return false;
		} finally {
			busy = undefined;
		}
	}

	// Undo for the intensity click (UI review U26): the change applies at once; for UNDO_MS after the
	// last change, Undo puts back the level before it. Each change pushes `{from, to}`, so two
	// changes in a row undo one step at a time. Intensity actions run one at a time (the control and
	// Undo are disabled meanwhile), so a step is never undone out of order. The 10 s wait pauses
	// while focus is on the Undo line. Nothing here changes what a level does.
	const UNDO_MS = 10_000;
	interface Step {
		from: Intensity;
		to: Intensity;
	}
	let undoStack = $state<Step[]>([]);
	// What the line names: the level now in force.
	let undoShown = $state<Intensity>('balanced');
	let undoTimer: ReturnType<typeof setTimeout> | undefined;
	let undoHeld = false;
	let disposed = false;
	function armUndo() {
		clearTimeout(undoTimer);
		if (disposed || undoHeld) return;
		undoTimer = setTimeout(dropUndo, UNDO_MS);
	}
	function dropUndo() {
		clearTimeout(undoTimer);
		undoStack = [];
		// The line is gone, so nothing inside it can hold focus: Firefox sends no `focusout` when the
		// focused button is removed, and a stale hold would stop the next line from expiring.
		undoHeld = false;
	}
	let intensityBusy = false;
	async function setIntensity(intensity: Intensity) {
		if (intensityBusy) return;
		intensityBusy = true;
		try {
			const from = crawler?.intensity;
			const ok = await act('intensity', () => api.patch('/crawler', { body: { intensity } }));
			if (!ok || disposed || from === undefined || from === intensity) return;
			undoStack = [...undoStack, { from, to: intensity }];
			undoShown = intensity;
			armUndo();
		} finally {
			intensityBusy = false;
		}
	}
	async function undoIntensity() {
		if (intensityBusy) return;
		const step = undoStack.at(-1);
		if (step === undefined) return;
		intensityBusy = true;
		try {
			const ok = await act('intensity', () =>
				api.patch('/crawler', { body: { intensity: step.from } })
			);
			// The line may have expired while the request ran: then nothing is announced.
			if (!ok || disposed || undoStack.at(-1) !== step) return;
			undoStack = undoStack.slice(0, -1);
			undoShown = step.from;
			if (undoStack.length) armUndo();
			else dropUndo();
		} finally {
			intensityBusy = false;
		}
	}
	/** Focus is on the Undo line: keep it; the 10 s starts again when focus leaves. */
	function holdUndo(held: boolean) {
		undoHeld = held;
		if (held) clearTimeout(undoTimer);
		else if (undoStack.length) armUndo();
	}

	/** Subscribes to the live feed; returns the unsubscribe. */
	function start(): () => void {
		disposed = false;
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
		return () => {
			offCrawler();
			offInsights();
			disposed = true;
			dropUndo();
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
		setIntensity,
		undoIntensity,
		/** The Undo line while it shows: the level now in force (a change or an undo alike). */
		get undo(): { level: Intensity } | undefined {
			return undoStack.length ? { level: undoShown } : undefined;
		},
		holdUndo,
		/** `'pending'` while the all-time check is unanswered; `'failed'` when it errored. */
		get startupCheck(): 'pending' | 'failed' | undefined {
			if (!overview) return undefined;
			if (startupState(query.range, overview, all) !== 'check') return undefined;
			return allFailed ? 'failed' : 'pending';
		}
	};
}

export type InsightsState = ReturnType<typeof createInsights>;
