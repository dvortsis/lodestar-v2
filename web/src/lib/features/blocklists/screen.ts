// Settings → Filters & Safety → Blocklist subscriptions (S-blocklists, §20.13). The entry is only
// the title and a forward to `body.ts`, which loads the screen (as Jobs does).
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'blocklists',
	group: 'library',
	title: () => m.blocklists_title(),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
