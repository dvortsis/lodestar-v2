<script lang="ts">
	// The overflow menu (§10.3): Import, Backup, Logs, Syntax guide, About. A separate file so the
	// menu primitive (Bits UI + positioning) loads on first use instead of with the page.
	import IconMore from '~icons/ph/dots-three';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import Menu from '$lib/ui/overlays/Menu.svelte';
	import { MORE, type Route } from './nav';

	interface Props {
		/** Adds the person's `density` to the link (URL state, §10.3). */
		hrefFor: (href: string) => string;
		/** Open straight away: the person already pressed the button that this one replaced. */
		openNow?: boolean;
	}
	let { hrefFor, openNow = false }: Props = $props();
	function onSelect(id: string) {
		const item = MORE.find((i) => i.id === id);
		if (item) void goto(resolve(hrefFor(item.href) as Route));
	}

	// svelte-ignore state_referenced_locally
	let open = $state(openNow);
</script>

<Menu
	bind:open
	label={m.shell_more_label()}
	items={MORE.map((i) => ({ id: i.id, label: i.label() }))}
	{onSelect}
>
	{#snippet trigger({ props })}
		<IconButton {...props} label={m.shell_more_label()} data-testid="more-button">
			<IconMore class="size-(--icon-size-nav)" />
		</IconButton>
	{/snippet}
</Menu>
