// The composition bar's inputs from a row's server-sent shares. Nothing is computed here: the
// shares are the server's whole percents, and "dominant" is the largest of them.
import type { Group } from '$lib/charts/shared';
import type { components } from '$lib/api/schema';
import { groupLabel, groupOf } from '$lib/features/detail/composition/groups';
import { m } from '$lib/paraglide/messages.js';

type Share = components['schemas']['CompositionShare'];

export interface BarInput {
	segments: { group: Group; value: number; label: string }[];
	summary: string;
	dominant: string;
}

/** Null when the torrent has no classified bytes yet ("Not classified yet"). */
export function composition(
	shares: Share[],
	formatShare: (pct: number, locale: string) => string,
	locale: string
): BarInput | null {
	if (shares.length === 0) return null;
	const known = shares.map((s) => ({ ...s, group: groupOf(s.group) }));
	const label = (s: (typeof known)[number]) =>
		m.dominant_share({ share: formatShare(s.pct_bytes, locale), group: groupLabel(s.group) });
	const top = known.reduce((a, b) => (b.pct_bytes > a.pct_bytes ? b : a));
	return {
		segments: known.map((s) => ({ group: s.group, value: s.bytes, label: groupLabel(s.group) })),
		summary: known.map(label).join(', '),
		dominant: label(top)
	};
}
