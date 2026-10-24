// Which torrent `c` and `o` act on. No imports, so the host and the search page can set it without
// pulling in the commands or the magnet code.
let drawerHash: string | null = null;
let rowHash: string | null = null;

/** The torrent of the open drawer (DetailHost sets and clears it). */
export function setDrawerTarget(hash: string | null) {
	drawerHash = hash;
}

/** The torrent of the focused result row, or null. Ignored while a drawer is open. */
export function setCommandTarget(hash: string | null) {
	rowHash = hash;
}

export const commandTarget = (): string | null => drawerHash ?? rowHash;
