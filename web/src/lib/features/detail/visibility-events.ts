// The settings subscription itself. Loaded on demand by visibility.ts, so the event client stays
// out of the drawer's and /t's first view (X-detail-headroom).
import { events } from '$lib/state/events.svelte';
import { clearDetailCaches } from './cache';

export function subscribeDetailVisibility() {
	events.on(
		'settings',
		(event) => {
			if (event.section === 'content') clearDetailCaches();
		},
		{ onResync: () => clearDetailCaches() }
	);
}
