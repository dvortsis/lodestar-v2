// The caches live for the tab's lifetime, so their single settings subscription does too.
// Loaded only when the detail feature is used; shares the existing SSE connection.
import { events } from '$lib/state/events.svelte';
import { clearDetailCaches } from './cache';

let installed = false;
export function watchDetailVisibility() {
	if (installed) return;
	installed = true;
	events.on(
		'settings',
		(event) => {
			if (event.section === 'content') clearDetailCaches();
		},
		{ onResync: () => clearDetailCaches() }
	);
}
