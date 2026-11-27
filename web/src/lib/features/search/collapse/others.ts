// "+3 other versions" (UI review 02 §5.1): the server's `others` on a grouped hit, worded. Shared
// by the List's disclosure, the Cards fact and the Table's Versions column.
import { formatCount } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';

export const othersText = (others: number, locale: string): string =>
	others === 1
		? m.search_collapse_others_one()
		: m.search_collapse_others_other({ count: formatCount(others, locale) });

/** The Table's Versions cell: every version in the group (the row's own and its others). */
export const versionsCell = (others: number | undefined, locale: string): string =>
	others === undefined ? '—' : formatCount(others + 1, locale);
