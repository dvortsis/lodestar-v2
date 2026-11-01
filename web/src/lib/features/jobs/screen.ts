// Settings → Crawling → Jobs (SRC-16, §18.8; runner decision Q17: a Settings screen, no new
// route). The entry stays this small because every Settings page loads it: the title only, the
// gear icon, and the body as a lazy import.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'jobs',
	group: 'crawling',
	title: () => m.jobs_title(),
	load: () => import('./JobsScreen.svelte')
};

export default screen;
