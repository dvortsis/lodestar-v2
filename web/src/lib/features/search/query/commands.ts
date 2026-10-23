// The search page's key commands (§10.16) in the `search` scope. The page passes in `$lib/keys`
// once the shell has loaded it (never a static import: W9 decisions). S-search-2 registers its own
// commands (j/k, o, c, s, x) in the same scope.
import type { keys as Keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';

export const SEARCH_SCOPE = 'search';

/** Activates the `search` scope and registers `search.focus` (`/`); returns the cleanup. */
export function registerSearchCommands(keys: typeof Keys, focus: () => void): () => void {
	const pop = keys.pushScope(SEARCH_SCOPE);
	const off = keys.register({
		id: 'search.focus',
		title: () => m.search_query_focus(),
		section: () => m.search_page_title(),
		scope: SEARCH_SCOPE,
		run: focus
	});
	return () => {
		off();
		pop();
	};
}
