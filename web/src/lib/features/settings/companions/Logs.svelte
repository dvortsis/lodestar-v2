<script lang="ts">
	// A companion's recent output, as text. A line is a text node, never markup.
	import { onMount } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { failureWords } from './logic';
	import type { CompanionsStore } from './store.svelte';

	let { store, id }: { store: CompanionsStore; id: string } = $props();
	let lines = $state<string[] | null>(null);
	let error = $state('');
	onMount(() => {
		store.logs(id).then(
			(l) => (lines = l),
			(e) => (error = failureWords(e))
		);
	});
</script>

{#if error}
	<Banner kind="error">{error}</Banner>
{:else if lines === null}
	<p class="text-small text-muted-text">{m.settings_companions_logs_loading()}</p>
{:else if lines.length === 0}
	<p class="text-small text-muted-text">{m.settings_companions_logs_empty()}</p>
{:else}
	<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
	<pre
		class="max-h-(--notice-max-height) overflow-auto whitespace-pre-wrap break-all rounded-selector border border-field-border p-2 font-mono text-caption"
		tabindex="0"
		aria-label={m.settings_companions_logs_label()}
		data-logs>{lines.join('\n')}</pre>
{/if}
