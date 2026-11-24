// Settings → Crawling → Add torrent files (UP-2, UI review 02 §5.3): upload `.torrent` files and set
// the watch folder. The entry stays this small because every Settings page loads it: the title and
// the hand-built body, which shows the watch folder's fields on its own form.
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'import',
	group: 'crawling',
	title: () => m.upload_title(),
	load: () => import('./UploadScreen.svelte')
};

export default screen;
