// Settings → Filters & Safety (S-filters, §20.7). The registry entry is as small as it can be: it
// costs bytes on every Settings first view and on the routes that share the registry's chunk. The
// purpose line is the body's heading line, and the navigation shows the gear (no `about`, no
// `iconLoad`: each is a lazy import whose preload list lands in this chunk).
import { m } from '$lib/paraglide/messages.js';
import type { Screen } from '../settings/shell/screens';

export const screen: Screen = {
	id: 'filters',
	group: 'library',
	title: () => m.filters_title(),
	load: () => import('./body').then((b) => b.load())
};

export default screen;
