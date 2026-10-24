<script lang="ts">
	// The field menu (⋯), loaded on first use: the Menu primitive brings Bits UI and its floating
	// positioning, which no field needs until someone opens a menu. Its items are built here, so
	// their labels load with it.
	import IconButton from '$lib/ui/controls/IconButton.svelte';
	import Menu, { type MenuItem } from '$lib/ui/overlays/Menu.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconMenu from '~icons/ph/dots-three';
	import type { FieldState } from './types';

	interface Props {
		field: FieldState;
		label: string;
		onSelect: (id: string) => void;
		open?: boolean;
	}

	let { field: f, label, onSelect, open = $bindable(false) }: Props = $props();

	const items = $derived<MenuItem[]>([
		...(f.source === 'db' && !f.locked && !f.spec.writeOnly
			? [{ id: 'reset', label: m.settings_form_menu_reset() }]
			: []),
		{ id: 'link', label: m.settings_form_menu_copy_link() },
		...(f.spec.config || f.spec.env
			? [{ id: 'keys', label: m.settings_form_menu_show_keys() }]
			: [])
	]);
</script>

<Menu {items} {onSelect} {label} bind:open>
	{#snippet trigger({ props })}
		<IconButton {...props} dense {label}>
			<IconMenu />
		</IconButton>
	{/snippet}
</Menu>
