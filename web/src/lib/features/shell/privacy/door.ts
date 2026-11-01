// The Overview's only reference to the privacy banner: a door with no dependencies, so the
// Overview's chunk lists one file to preload, not the banner's whole graph (§10.1).
import type { Component } from 'svelte';
import type { components } from '$lib/api/schema';

export interface BannerProps {
	/** The overview response: its `privacy` summary and `updated_at` (server Unix seconds). */
	overview: { privacy: components['schemas']['PrivacySummary'] | null; updated_at: number };
	/** Reads the summary again (after Turn off now, and once the time runs out). */
	reload: () => void;
}

export const load = () =>
	import('./PrivacyBanner.svelte') as unknown as Promise<{ default: Component<BannerProps> }>;

/** What `load` resolves to, for a caller's `.then<Loaded>()`. */
export type Loaded = Awaited<ReturnType<typeof load>>;
