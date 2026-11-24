// One torrent's siblings, fetched for the Related section. A 404 means the torrent is unknown or
// withheld from view (hidden, adult with Show adult off, private): the section then shows nothing at
// all, so nothing the API withheld leaks through it. A load that outlives a content-settings change
// (the detail caches' generation moved) is dropped, never shown.
import { ApiError, api, isAbort } from '$lib/api/client';
import { detailGeneration } from '../cache';
import type { Siblings } from './lists';

export type Loaded =
	| { status: 'loading' }
	| { status: 'ready'; siblings: Siblings }
	| { status: 'withheld' }
	| { status: 'failed' };

/** Loads `hash`'s siblings and reports each state to `set`; returns a function that cancels it. */
export function loadSiblings(hash: string, set: (state: Loaded) => void): () => void {
	const ctl = new AbortController();
	const generation = detailGeneration();
	const current = () => !ctl.signal.aborted && generation === detailGeneration();
	set({ status: 'loading' });
	// Through a resolved promise, so a client that throws at once fails the same way as a request.
	Promise.resolve()
		.then(() => api.get('/torrents/{hash}/siblings', { path: { hash }, signal: ctl.signal }))
		.then((siblings) => {
			if (!current()) return;
			// The answer must be about the torrent asked for.
			if (siblings?.hash !== hash) set({ status: 'failed' });
			else set({ status: 'ready', siblings });
		})
		.catch((e: unknown) => {
			if (isAbort(e) || !current()) return;
			set({ status: e instanceof ApiError && e.status === 404 ? 'withheld' : 'failed' });
		});
	return () => ctl.abort();
}
