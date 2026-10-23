// The composition bar's inputs from a row's server-sent shares. Nothing is computed here: the
// shares are the server's whole percents, and "dominant" is the largest of them.
import type { Group } from '$lib/charts';
import { m } from '$lib/paraglide/messages.js';
import type { components } from '$lib/api/schema';
import { GROUP_ORDER } from '$lib/charts';

type Share = components['schemas']['CompositionShare'];

// TODO(S-detail): the group labels become shared messages when S-detail adds them.
const GROUPS: Record<Group, () => string> = {
	video: () => m.discover_group_video(),
	audio: () => m.discover_group_audio(),
	images: () => m.discover_group_images(),
	documents: () => m.discover_group_documents(),
	archives: () => m.discover_group_archives(),
	software: () => m.discover_group_software(),
	other: () => m.discover_group_other()
};

const isGroup = (g: string): g is Group => (GROUP_ORDER as readonly string[]).includes(g);

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
	const known = shares.filter((s): s is Share & { group: Group } => isGroup(s.group));
	if (known.length === 0) return null;
	const label = (s: Share & { group: Group }) =>
		m.discover_dominant_share({
			share: formatShare(s.pct_bytes, locale),
			group: GROUPS[s.group]()
		});
	const top = known.reduce((a, b) => (b.pct_bytes > a.pct_bytes ? b : a));
	return {
		segments: known.map((s) => ({ group: s.group, value: s.bytes, label: GROUPS[s.group]() })),
		summary: known.map(label).join(', '),
		dominant: label(top)
	};
}
