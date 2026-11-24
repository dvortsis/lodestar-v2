// Settings → Crawling → Add torrent files (UP-2, UI review 02 §5.3): upload `.torrent` files and set
// the watch folder. The entry stays this small because every Settings page loads it: the title and
// a forward to `body.ts`, which loads the screen (as Sources does), so the body's preload list
// stays out of the registry's chunk.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'import',
	group: 'crawling',
	title: () => m.upload_title(),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
