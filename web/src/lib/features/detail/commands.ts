// `c` copies and `o` opens the magnet of the open drawer's torrent, else of whatever the search
// page's focused row set with `setCommandTarget`. S-search-2 must not register these ids again.
import { m } from '$lib/paraglide/messages.js';
import { copyMagnet, openMagnet } from './magnet';

let drawerHash: string | null = null;
let rowHash: string | null = null;
let registered: Promise<void> | undefined;

const target = () => drawerHash ?? rowHash;

/** The torrent of the open drawer (DetailHost sets and clears it). */
export function setDrawerTarget(hash: string | null) {
	drawerHash = hash;
}

/** The torrent of the focused result row, or null. Ignored while a drawer is open. */
export function setCommandTarget(hash: string | null) {
	rowHash = hash;
}

export const commandTarget = target;

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
				if (hash) void copyMagnet({ hash });
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
				if (hash) void openMagnet({ hash });
			}
		});
	});
	return registered;
}
