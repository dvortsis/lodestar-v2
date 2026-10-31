<script lang="ts">
	// "Removing {clause} gives {N}" (§10.4 No results): one button per counted clause, each one
	// search of the server's reduced query. Loaded only for an empty answer that carries counts.
	import type { components } from '$lib/api/schema';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { ensureSearchPath } from '../query/path';
	import { suggestions, type Suggestion } from './suggestions';

	interface Props {
		recovery: components['schemas']['SearchRecovery'] | null;
	}
	let { recovery }: Props = $props();
	const items = $derived(suggestions(recovery));
	async function apply(item: Suggestion) {
		ensureSearchPath();
		await search.setAst(item.without);
	}
	const locale = $derived(currentLocale());
</script>

{#if items.length}
	<ul class="rp-examples" data-testid="results-recover">
		{#each items as item, i (i)}
			<li>
				<Button dense onclick={() => void apply(item)}
					>{m.search_recover_gives({
						clause: item.clause,
						count: formatCount({ value: item.total, approx: item.approx }, locale)
					})}</Button
				>
			</li>
		{/each}
	</ul>
{/if}
