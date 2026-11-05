// Settings → Crawling → Sources (SRC-15a, §18.8, §10.10). The entry is only the title and a forward
// to `body.ts`, which loads the screen (as Jobs and Blocklists do).
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'sources',
	group: 'crawling',
	title: () => m.sources_title(),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
