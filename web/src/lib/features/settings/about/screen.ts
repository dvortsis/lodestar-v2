// Settings → About (S-about, §10.10.13). The icon and the body load on their own, so every Settings
// page carries only this entry and its title; the purpose line is in the lazy `about`.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screen: Screen = {
	id: 'about',
	group: 'system',
	title: () => m.settings_about_title(),
	about: () => import('./about'),
	iconLoad: () => import('./icon'),
	load: () => import('./AboutScreen.svelte')
};

export default screen;
