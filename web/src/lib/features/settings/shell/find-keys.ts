// `/` focuses the Settings search box while a Settings page is open (§10.10.3). The key goes
// through the keys registry in a `settings` scope, so it shows in the shortcuts help and never
// shares the key with Search's own `/` (that one is in the `search` scope, which isn't active here).
import { keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';
export { NAV_ICONS } from './nav-icons';

export const SETTINGS_SCOPE = 'settings';

/** Activates the `settings` scope and registers `settings.find`; returns the cleanup. */
export function registerFindKey(box: () => HTMLElement | undefined): () => void {
	const pop = keys.pushScope(SETTINGS_SCOPE);
	const off = keys.register({
		id: 'settings.find',
		title: () => m.settings_find_key(),
		section: () => m.settings_shell_title(),
		keys: ['/'],
		scope: SETTINGS_SCOPE,
		// Only where the box is on screen: below 1024 px it shows on the Overview alone.
		when: () => (box()?.getClientRects().length ?? 0) > 0,
		run: () => box()?.focus()
	});
	return () => {
		off();
		pop();
	};
}

let panel: Promise<typeof import('./FindPanel.svelte')> | undefined;
/** The results panel, loaded once (the shell asks when the box is first focused). */
export const loadPanel = () =>
	(panel ??= import('./FindPanel.svelte').catch((e: unknown) => {
		panel = undefined; // tried again the next time the box is focused
		throw e;
	}));
