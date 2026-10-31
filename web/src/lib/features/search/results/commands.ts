// The results' keys (§10.16), in the page's `search` scope: j/k move through the torrents (Enter
// opens one: VirtualList's own). `c` and `o` belong to S-detail (`registerDetailCommands`): they act
// on the row this list hands to `setCommandTarget`, so they are not registered here.
import type { keys as Keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';
import { SEARCH_SCOPE } from '../query/commands';

export function registerResultsCommands(
	keys: typeof Keys,
	move: { next: () => void; previous: () => void; select: () => void }
): () => void {
	const offs = [
		keys.register({
			id: 'results.next',
			title: () => m.search_results_next(),
			section: () => m.search_page_title(),
			scope: SEARCH_SCOPE,
			run: move.next
		}),
		keys.register({
			id: 'results.previous',
			title: () => m.search_results_previous(),
			section: () => m.search_page_title(),
			scope: SEARCH_SCOPE,
			run: move.previous
		}),
		// `x` selects the active row (S-search-5; the default key is `results.select`).
		keys.register({
			id: 'results.select',
			title: () => m.search_bulk_mode(),
			section: () => m.search_page_title(),
			scope: SEARCH_SCOPE,
			run: move.select
		})
	];
	return () => offs.forEach((off) => off());
}
