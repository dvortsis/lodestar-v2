// Settings → Updates (S-updates). The icon, the purpose line and the body load on their own
// (icon.ts, about.ts, body.ts), so every Settings page carries only this entry and its title.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screen: Screen = {
	id: 'updates',
	group: 'system',
	title: () => m.settings_updates_title(),
	about: () => import('./about').then((a) => a.ABOUT),
	iconLoad: () => import('./icon'),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
