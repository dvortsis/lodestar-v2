// The command palette's "Settings: …" entries (X-settings-find). Loaded by the palette on the first
// search, so the palette itself carries only the call.
import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { m } from '$lib/paraglide/messages.js';
import type { Route } from '$lib/shell/nav';
import { onSessionOk } from '$lib/api/client';
import { highlight, loadFindIndex, search } from './find';

/** The fields that match, as palette options; none when the index can't load (signed out, offline). */
let failedAt = 0;
// The last outline, stopped when another result is opened.
let stopHighlight: (() => void) | undefined;
// Signing in (any request that works) lifts the back-off.
onSessionOk(() => (failedAt = 0));

export async function settingsRows(query: string) {
	// After a failure (signed out, offline) the index isn't asked for again for a while, so typing
	// doesn't repeat the request on every key.
	if (Date.now() - failedAt < 30_000) return [];
	let entries;
	try {
		entries = await loadFindIndex();
	} catch {
		failedAt = Date.now();
		return [];
	}
	return search(entries, query, 8).map((entry) => ({
		id: `settings:${entry.href}`,
		title: m.settings_find_palette_title({ label: entry.label }),
		section: entry.trail,
		keys: [] as string[],
		open: () => {
			stopHighlight?.();
			void goto(resolve(entry.href as Route)).then(() => {
				if (entry.spec) stopHighlight = highlight(entry.spec.key);
			});
		}
	}));
}
