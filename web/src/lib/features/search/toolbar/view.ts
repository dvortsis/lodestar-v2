// The one view (List, Compact, Table, Cards) the rows, the table and the toolbar all read. Pure and
// message-free: the results panel imports it too, so it holds only what the panel needs (the
// toolbar's own helpers, and the account preference's seeding, are in toolbar.ts).
export type View = 'list' | 'compact' | 'table' | 'cards';

/**
 * The one view: an explicit `view=table`, then an explicit `density=` (`comfortable` is the List),
 * then the account's Appearance density (`pref`, §10.10: Compact or Cards; Comfortable is the List),
 * then the List. Old links keep working. Nothing writes the URL until the user picks a view.
 */
export const viewOf = (url: URL, pref?: string): View => {
	const p = url.searchParams;
	const d = p.get('density') ?? pref;
	return p.get('view') === 'table' ? 'table' : d === 'cards' || d === 'compact' ? d : 'list';
};
