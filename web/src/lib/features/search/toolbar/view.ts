// The one view (List, Compact, Table, Cards) from the query string and the account's density. Pure
// and message-free. The results panel (results/ResultsPanel.svelte) applies the same rule inline,
// because that chunk has no room to import it: change both together.
export type View = 'list' | 'compact' | 'table' | 'cards';

/**
 * The one view: an explicit `view=table`, then an explicit `density=` (`comfortable` is the List),
 * then the account's Appearance density (`pref`, §10.10: Compact or Cards; Comfortable is the List),
 * then the List. Old links keep working. Nothing writes the URL until the user picks a view.
 */
export const viewOf = (search: string, pref?: string): View => {
	const p = new URLSearchParams(search);
	const d = p.get('density') ?? pref;
	return p.get('view') === 'table' ? 'table' : d === 'cards' || d === 'compact' ? d : 'list';
};
