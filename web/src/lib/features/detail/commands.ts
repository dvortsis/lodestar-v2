// `c` copies and `o` opens the magnet of the open drawer's torrent, else of whatever the search
// page's focused row set with `setCommandTarget`. S-search-2 must not register these ids again.
import { m } from '$lib/paraglide/messages.js';

import { commandTarget as target } from './target';

export { commandTarget, setCommandTarget, setDrawerTarget } from './target';

let registered: Promise<void> | undefined;

/** Idempotent: a second call returns the first registration. */
export function registerDetailCommands(): Promise<void> {
	registered ??= import('$lib/keys').then(({ keys }) => {
		const when = () => target() !== null;
		keys.register({
			id: 'detail.copy_magnet',
			title: () => m.detail_copy_magnet(),
			section: () => m.torrent(),
			keys: ['c'],
			when,
			run: () => {
				const hash = target();
				if (hash) void import('./magnet').then((mod) => mod.copyMagnet({ hash })).catch(() => {});
			}
		});
		keys.register({
			id: 'detail.open_magnet',
			title: () => m.detail_open_magnet(),
			section: () => m.torrent(),
			keys: ['o'],
			when,
			run: () => {
				const hash = target();
				if (hash) void import('./magnet').then((mod) => mod.openMagnet({ hash })).catch(() => {});
			}
		});
	});
	return registered;
}
