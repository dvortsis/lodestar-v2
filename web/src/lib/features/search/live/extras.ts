// What the results panel loads on demand (S-search-6): the live banner once there are rows, the
// Cards list when the density is Cards. One entry, so the panel carries a single dynamic import.
export async function load(want: { cards: boolean; live: boolean }) {
	const [Live, Cards] = await Promise.all([
		want.live ? import('./LiveHost.svelte').then((m) => m.default) : undefined,
		want.cards ? import('../cards/CardList.svelte').then((m) => m.default) : undefined
	]);
	return { Live, Cards };
}
