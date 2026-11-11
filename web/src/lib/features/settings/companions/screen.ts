// Settings → Companions (CMP-9a). The icon, the purpose line and the body load on their own
// (icon.ts, about.ts, body.ts), so every Settings page carries only this entry and its title.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screen: Screen = {
	id: 'companions',
	group: 'crawling',
	title: () => m.settings_companions_title(),
	about: () => import('./icon').then((a) => a.ABOUT),
	iconLoad: () => import('./icon'),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
