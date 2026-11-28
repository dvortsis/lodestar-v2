// The row's badges (§10.4, UI review U11): what the torrent is, then the flags people filter by.
// With more than five, safety and verification badges never give way: the rest fill the free
// slots in order and the remainder collapse into one "+N" chip (runner, owner item 28).
import type { SearchHit } from './ResultRow.svelte';

export const MAX_BADGES = 5;

/** The window event a clicked badge sends: `detail` is `{ facet, value }` (the rail's names). */
export const ROW_FILTER = 'ls-row-filter';

export type BadgeKind = 'text' | 'unverified' | 'suspect';

export interface RowBadge {
	key: string;
	kind: BadgeKind;
	/** Text for `text` kinds (the other kinds draw their own). */
	label: string;
	/** Safety and verification: always shown, never counted into "+N". */
	keep: boolean;
	/** A click filters by this: the rail's facet name and value (toggle.ts `fieldOf`). */
	filter?: { facet: string; value: string };
}

/** The labels a row needs, passed in so this module stays free of message imports. */
export interface BadgeLabels {
	type: (key: string, adultKind: string | null) => string | undefined;
	private: string;
	imported: string;
	v2: string;
	hybrid: string;
}

export function badgesOf(hit: SearchHit, l: BadgeLabels): RowBadge[] {
	const out: RowBadge[] = [];
	const adult = hit.content_type === 'adult' && hit.adult_kind ? hit.adult_kind : null;
	const type = hit.content_type ? l.type(hit.content_type, adult) : undefined;
	if (type && hit.content_type) {
		const filter = adult
			? { facet: 'adult', value: adult }
			: { facet: 'type', value: hit.content_type };
		// An adult torrent's kind is a content-safety fact (§20): it stays.
		out.push({ key: 'type', kind: 'text', label: type, keep: adult !== null, filter });
	}
	if (hit.resolution)
		out.push({
			key: 'res',
			kind: 'text',
			label: hit.resolution,
			keep: false,
			filter: { facet: 'res', value: hit.resolution }
		});
	if (hit.flags.includes('private'))
		out.push({
			key: 'private',
			kind: 'text',
			label: l.private,
			keep: false,
			filter: { facet: 'private', value: 'yes' }
		});
	// An imported torrent that is also unverified reads once, as the verification badge.
	if (hit.flags.includes('imported') && hit.verified !== false)
		out.push({
			key: 'imported',
			kind: 'text',
			label: l.imported,
			keep: false,
			filter: { facet: 'imported', value: 'yes' }
		});
	// Technical (U11): last of the choice badges, so the first into "+N"; the drawer holds both.
	if (hit.flags.includes('v2'))
		out.push({
			key: 'v2',
			kind: 'text',
			label: l.v2,
			keep: false,
			filter: { facet: 'v2', value: 'yes' }
		});
	if (hit.flags.includes('hybrid'))
		out.push({ key: 'hybrid', kind: 'text', label: l.hybrid, keep: false });
	if (hit.verified === false)
		out.push({ key: 'unverified', kind: 'unverified', label: '', keep: true });
	if (hit.swarm_suspect) out.push({ key: 'suspect', kind: 'suspect', label: '', keep: true });
	return out;
}

/**
 * At most `max` badges as shown, in their own order. Over that, one slot goes to the "+N" chip;
 * every `keep` badge is shown first, normal badges fill what is left. `hidden` is what the chip
 * counts (null: no chip).
 */
export function collapseBadges(
	all: RowBadge[],
	max = MAX_BADGES
): { shown: RowBadge[]; hidden: number | null } {
	if (all.length <= max) return { shown: all, hidden: null };
	let free = Math.max(0, max - 1 - all.filter((b) => b.keep).length);
	const shown = all.filter((b) => {
		if (b.keep) return true;
		if (free > 0) {
			free -= 1;
			return true;
		}
		return false;
	});
	const hidden = all.length - shown.length;
	return { shown, hidden: hidden > 0 ? hidden : null };
}
