// The toolbar's count while the rows are groups (UI review 02 §5.1): "128 groups · 412 torrents".
// Both numbers are the server's (§10.4: the client never counts); an estimate reads "≈ 2.4M", as
// the ungrouped count does.
import { formatCompact, formatCount } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';

const figure = (n: number, approx: boolean, locale: string) =>
	approx ? formatCompact({ value: n, approx: true }, locale) : formatCount(n, locale);

export function groupsCount(
	groups: number,
	groupsApprox: boolean,
	total: number,
	totalApprox: boolean,
	locale: string
): string {
	if (groups === 1 && total === 1 && !groupsApprox && !totalApprox)
		return m.search_collapse_count_single();
	const torrents = figure(total, totalApprox, locale);
	return groups === 1 && !groupsApprox
		? m.search_collapse_count_one({ torrents })
		: m.search_collapse_count({ groups: figure(groups, groupsApprox, locale), torrents });
}
