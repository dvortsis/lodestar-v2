import type { createSearchState } from '$lib/state/search.svelte';

/** W5's search state (`search`, or a test's `createSearchState(nav)`). */
export type SearchState = ReturnType<typeof createSearchState>;

/** What the bar's lazy part offers the static bar. */
export interface LiveApi {
	focusEnd(): void;
}
