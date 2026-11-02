// Settings → Crawling → Jobs (SRC-16, §18.8; runner decision Q17: a Settings screen, no new
// route). The entry stays this small because every Settings page loads it: the title and a
// forward to `body.ts`, which loads the screen.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'jobs',
	group: 'crawling',
	title: () => m.jobs_title(),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
