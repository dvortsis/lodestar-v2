// Which torrent `c` and `o` act on. No imports, so the host and the search page can set it without
// pulling in the commands or the magnet code.
export let drawerHash: string | null = null;
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

// The torrent the drawer or /t has finished loading, set by the model. With a drawer or /t open,
// `s` waits for it (send.ts); `c` and `o` fetch what they need themselves and act at once.
export let loadedHash: string | null = null;
export const setLoadedHash = (hash: string | null) => (loadedHash = hash);
