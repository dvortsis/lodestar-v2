// The screen loads as its own chunk, so the route node stays as small as the other routes' stubs:
// SvelteKit lists every node's chunks in the entry every page loads (§10.1 headroom, W9 decisions).
export const load = async () => ({
	Compare: (await import('$lib/features/compare/Compare.svelte')).default
});
