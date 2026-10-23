<script lang="ts">
	// The channels the API has: "In the app" (`alert`) and "RSS" (`rss`). The notifier's external
	// channels are stored but neither returned nor settable yet (B10a), so they aren't shown.
	// Icon + text, never colour alone.
	import IconBell from '~icons/ph/bell-simple';
	import IconRss from '~icons/ph/rss';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { SavedSearch } from './api';

	let { search }: { search: Pick<SavedSearch, 'alert' | 'rss'> } = $props();
</script>

<span class="al-channels">
	{#if search.alert}
		<Badge><IconBell class="size-4 shrink-0" aria-hidden="true" />{m.alerts_channel_app()}</Badge>
	{/if}
	{#if search.rss}
		<Badge><IconRss class="size-4 shrink-0" aria-hidden="true" />{m.alerts_channel_rss()}</Badge>
	{/if}
	{#if !search.alert && !search.rss}
		<span class="al-muted">{m.alerts_channel_none()}</span>
	{/if}
</span>

<style>
	.al-channels {
		display: inline-flex;
		flex-wrap: wrap;
		gap: calc(var(--spacing) * 1);
		align-items: center;
	}
	.al-muted {
		color: var(--color-muted-text);
	}
</style>
