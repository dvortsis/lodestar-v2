// Settings → Security (M-security-settings). The icon and the body load on their own (icon.ts,
// body.ts), so every Settings page carries only this entry and its title.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screen: Screen = {
	id: 'security',
	group: 'system',
	title: () => m.settings_security_title(),
	iconLoad: () => import('./icon'),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
