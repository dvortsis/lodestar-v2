// The search page's selection (§10.4): the rows picked for the bulk bar, by info hash. It survives
// appended pages (the same query) and clears when the query changes (`follow`). Select mode shows
// the checkboxes. Range and select-all live in `ops.ts`, loaded with the checkboxes.
export function createSelection() {
	let hashes = $state.raw<string[]>([]);
	let mode = $state(false);
	let shown: string | null = null;
	// A derived lookup, rebuilt from `hashes` and never mutated.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const set = $derived(new Set(hashes));

	return {
		get hashes() {
			return hashes;
		},
		get count() {
			return hashes.length;
		},
		get mode() {
			return mode;
		},
		set mode(on: boolean) {
			mode = on;
			if (!on) hashes = [];
		},
		has: (hash: string) => set.has(hash),
		/** Replace the selection; a non-empty one turns select mode on. */
		put(next: string[]) {
			hashes = next;
			if (next.length) mode = true;
		},
		toggle(hash: string, on = !set.has(hash)) {
			this.put(
				on ? (set.has(hash) ? hashes : [...hashes, hash]) : hashes.filter((h) => h !== hash)
			);
			if (on) mode = true;
		},
		/** The query the rows answer (`results.shown`): a different one starts a new selection. */
		follow(key: string | null) {
			if (key === shown) return;
			shown = key;
			hashes = [];
		}
	};
}

export const selection = createSelection();
