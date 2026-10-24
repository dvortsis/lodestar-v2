// The rail's pending edit: a click handed to W5 that W5 hasn't adopted yet (its `ast` changes only
// once the server printed the edit back). A second click builds on it rather than on the older
// query. It's dropped when W5's query moves for any reason: its `ast` (the edit adopted, or a new
// parse), or its `text` (typing, even when the new text fails to parse and `ast` stays null).
import type { Ast } from '$lib/state/search.svelte';

export function createPending(w5: { ast: () => Ast | null; text: () => string }) {
	let pending = $state.raw<Ast | null>(null);
	$effect(() => {
		w5.ast();
		w5.text();
		pending = null;
	});
	return {
		get value() {
			return pending;
		},
		set(next: Ast) {
			pending = next;
		},
		/** W5 refused `next`: edit W5's own query again, unless a newer click replaced it. */
		drop(next: Ast) {
			if (pending === next) pending = null;
		}
	};
}
