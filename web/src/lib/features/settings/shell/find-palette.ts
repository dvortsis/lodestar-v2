// The command palette's "Settings: …" entries (X-settings-find). Loaded by the palette on the first
// search, so the palette itself carries only the call.
import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { m } from '$lib/paraglide/messages.js';
import type { Route } from '$lib/shell/nav';
import { highlight, loadFindIndex, search } from './find';

/** The fields that match, as palette options; none when the index can't load (signed out, offline). */
export async function settingsRows(query: string) {
	return search(await loadFindIndex(), query, 8).map((entry) => ({
		id: `settings:${entry.href}`,
		title: m.settings_find_palette_title({ label: entry.label }),
		section: entry.trail,
		keys: [] as string[],
		open: () => {
			void goto(resolve(entry.href as Route)).then(() => {
				if (entry.spec) highlight(entry.spec.key);
			});
		}
	}));
}
