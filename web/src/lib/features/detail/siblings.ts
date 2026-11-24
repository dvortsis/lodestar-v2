// One torrent's siblings (`GET /torrents/{hash}/siblings`), shared by the sections that read them:
// Parsed (the title identity's spans) and Related (the four lists). Both mount on the same torrent,
// so they share one request: a load already in flight is joined, and an answer stays good for a
// minute (`FRESH_MS`). A Retry asks again. A 404 means the torrent is unknown or withheld from view
// (hidden, adult with Show adult off, private): the sections then show nothing of it, so nothing
// the API withheld leaks through them. An answer about another hash is a failure. A load that
// outlives a content-settings change (the detail caches' generation moved) is dropped, never shown,
// and nothing answered before that change is reused after it.
import { ApiError, api, isAbort } from '$lib/api/client';
import { FRESH_MS, detailGeneration } from './cache';
import type { Siblings } from './related/lists';

export type Loaded =
	| { status: 'loading' }
	| { status: 'ready'; siblings: Siblings }
	| { status: 'withheld' }
	| { status: 'failed' };

type Settled = Exclude<Loaded, { status: 'loading' }>;
interface Flight {
	generation: number;
	ctl: AbortController;
	listeners: Set<(state: Settled) => void>;
}

const LIMIT = 8;
const flights = new Map<string, Flight>();
/** Answers, newest last: only ready ones, each with the generation and time it was answered in. */
const answers = new Map<string, { generation: number; at: number; siblings: Siblings }>();

function fly(hash: string): Flight {
	const flight: Flight = {
		generation: detailGeneration(),
		ctl: new AbortController(),
		listeners: new Set()
	};
	flights.set(hash, flight);
	const { signal } = flight.ctl;
	const finish = (state: Settled) => {
		if (flights.get(hash) === flight) flights.delete(hash);
		if (signal.aborted || flight.generation !== detailGeneration()) return;
		if (state.status === 'ready') {
			answers.delete(hash);
			answers.set(hash, {
				generation: flight.generation,
				at: Date.now(),
				siblings: state.siblings
			});
			while (answers.size > LIMIT) answers.delete(answers.keys().next().value as string);
		}
		for (const fn of [...flight.listeners]) fn(state);
	};
	// Through a resolved promise, so a client that throws at once fails the same way as a request.
	// The answer and the failure are separate handlers: a section that throws on a ready answer must
	// not turn it into a failure for every other section.
	Promise.resolve()
		.then(() => api.get('/torrents/{hash}/siblings', { path: { hash }, signal }))
		.then(
			(siblings) =>
				// The answer must be about the torrent asked for.
				finish(siblings?.hash === hash ? { status: 'ready', siblings } : { status: 'failed' }),
			(e: unknown) => {
				if (isAbort(e)) return;
				finish({ status: e instanceof ApiError && e.status === 404 ? 'withheld' : 'failed' });
			}
		);
	return flight;
}

/**
 * Loads `hash`'s siblings and reports each state to `set`; returns a function that stops reporting
 * (the request is cancelled once nobody waits for it). `again` skips a kept answer (a Retry).
 */
export function loadSiblings(
	hash: string,
	set: (state: Loaded) => void,
	again = false
): () => void {
	const generation = detailGeneration();
	const kept = answers.get(hash);
	if (!again && kept?.generation === generation && Date.now() - kept.at <= FRESH_MS) {
		set({ status: 'ready', siblings: kept.siblings });
		return () => {};
	}
	set({ status: 'loading' });
	const now = flights.get(hash);
	const flight = now?.generation === generation ? now : fly(hash);
	flight.listeners.add(set);
	return () => {
		flight.listeners.delete(set);
		if (flight.listeners.size === 0) {
			flight.ctl.abort();
			if (flights.get(hash) === flight) flights.delete(hash);
		}
	};
}
