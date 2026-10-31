<script lang="ts">
	// "Removing {clause} gives {N}" (§10.4 No results): one button per counted clause, each one
	// search of the server's reduced query. Loaded only for an empty answer that carries counts.
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import type { Suggestion } from './recover';

	interface Props {
		items: Suggestion[];
		apply: (item: Suggestion) => void;
	}
	let { items, apply }: Props = $props();
	const locale = $derived(currentLocale());
</script>

<ul class="rp-examples" data-testid="results-recover">
	{#each items as item, i (i)}
		<li>
			<Button dense onclick={() => apply(item)}
				>{(item.approx ? m.search_recover_gives_about : m.search_recover_gives)({
					clause: item.clause,
					count: formatCount(item.total, locale)
				})}</Button
			>
		</li>
	{/each}
</ul>
