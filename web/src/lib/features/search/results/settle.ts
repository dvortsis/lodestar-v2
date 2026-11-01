// After "Block this…" applied: reload as many rows as were shown (the scroll holds), say how many
// went, and put focus on the row that took the blocked row's slot (the next, or the previous if it
// was last), or on the results heading when none is left. Loaded only after an apply.
import { tick } from 'svelte';
import { m } from '$lib/paraglide/messages.js';
import { results } from './results.svelte';

export async function settle(hash: string) {
	const at = results.hits.findIndex((h) => h.hash === hash);
	const had = results.hits.length;
	await results.reload(had);
	const rows = results.hits;
	const gone = had - rows.length;
	results.note =
		gone > 1
			? m.search_results_blocked_other({ count: gone })
			: gone === 1
				? m.search_results_blocked_one()
				: '';
	await tick();
	const next = rows[Math.min(Math.max(at, 0), rows.length - 1)];
	const row = next && document.querySelector(`[data-hash="${next.hash}"]`);
	(row?.closest<HTMLElement>('[data-index]') ?? document.getElementById('rp-label'))?.focus({
		preventScroll: true
	});
}
