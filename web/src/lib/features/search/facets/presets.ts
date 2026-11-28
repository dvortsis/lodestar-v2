// Seeder and tracker presets (§10.4, UI review I6): plain `>=` clauses that work whether or not
// the server has swarm counts yet. A server bucket value (`≥5`) supplies a count; a preset it
// doesn't list still shows, without one.
import type { TermFacet } from './facets.svelte';

export const SWARM_PRESETS = {
	seeders: [1, 5, 50],
	trackers: [1, 3]
} as const;

export interface SwarmPreset {
	n: number;
	/** The server's count for `≥ n`, or null when it has none (never a zero in its place). */
	count: number | null;
}

export function swarmPresets(
	name: 'seeders' | 'trackers',
	bucket: TermFacet | null
): SwarmPreset[] {
	const counts = new Map<number, number>();
	for (const v of bucket?.values ?? []) {
		const n = /^(?:≥|>=)\s*(\d+)$/.exec(v.value)?.[1];
		if (n) counts.set(Number(n), v.count);
	}
	return SWARM_PRESETS[name].map((n) => ({ n, count: counts.get(n) ?? null }));
}
