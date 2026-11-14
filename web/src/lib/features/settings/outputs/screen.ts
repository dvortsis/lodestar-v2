// Settings → Outputs (OUT-2): the Torznab address and keys, and the bitmagnet export. The icon,
// the purpose line and the body load on their own (icon.ts, which also holds the purpose line,
// and body.ts), so every Settings page carries only this entry and its title.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../shell/screens';

export const screen: Screen = {
	id: 'outputs',
	group: 'connections',
	title: () => m.settings_outputs_title(),
	about: () => import('./icon').then((a) => a.ABOUT),
	iconLoad: () => import('./icon'),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
