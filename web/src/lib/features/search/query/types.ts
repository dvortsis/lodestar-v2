import type { Component } from 'svelte';
import type { createSearchState } from '$lib/state/search.svelte';

/** W5's search state (`search`, or a test's `createSearchState(nav)`). */
export type SearchState = ReturnType<typeof createSearchState>;

/** What the bar's lazy part offers the static bar. */
export interface LiveApi {
	/** Sets the query and submits it (the Syntax popover's "Try"). */
	tryQuery(query: string): void;
	focusEnd(): void;
	/** The Syntax popover (its own chunk, loaded on first open). */
	loadSyntax(): Promise<SyntaxComponent>;
}

export type SyntaxComponent = Component<
	{ open?: boolean; ontry: (query: string) => void },
	object,
	'open'
>;
