<script lang="ts">
	// The field menu (⋯), loaded on first use: the Menu primitive brings Bits UI and its floating
	// positioning, which no field needs until someone opens a menu.
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import Menu, { type MenuItem } from '$lib/ui/overlays/Menu.svelte';
	import IconMenu from '~icons/ph/dots-three';

	interface Props {
		items: MenuItem[];
		label: string;
		onSelect: (id: string) => void;
		open?: boolean;
	}

	let { items, label, onSelect, open = $bindable(false) }: Props = $props();
</script>

<Menu {items} {onSelect} {label} bind:open>
	{#snippet trigger({ props })}
		<IconButton {...props} dense {label}>
			<IconMenu />
		</IconButton>
	{/snippet}
</Menu>
