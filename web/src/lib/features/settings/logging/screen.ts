// Settings → Logging (S-logs-2, §10.10.12). The icon and the body load on their own, so every
// Settings page carries only this entry and its title.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screen: Screen = {
	id: 'logging',
	group: 'system',
	title: () => m.settings_logging_title(),
	iconLoad: () => import('./icon'),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
