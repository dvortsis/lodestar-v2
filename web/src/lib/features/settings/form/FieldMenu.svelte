<script lang="ts">
	// The field menu (⋯), loaded on first use: the Menu primitive brings Bits UI and its floating
	// positioning, which no field needs until someone opens a menu. Its items are built here, so
	// their labels load with it.
	import { untrack } from 'svelte';
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
		/**
		 * The menu's first open was made from the keyboard, before this lazily loaded menu was mounted (Bits UI
		 * only focuses the first item itself once it has seen a keydown). Later opens go through the mounted trigger.
		 */
		byKeyboard?: boolean;
	}

	let { field: f, label, onSelect, open = $bindable(false), byKeyboard = false }: Props = $props();
	let autofocusFirst = $state(untrack(() => byKeyboard));
	$effect(() => {
		if (!open) autofocusFirst = false;
	});

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

<Menu {items} {onSelect} {label} {autofocusFirst} bind:open>
	{#snippet trigger({ props })}
		<IconButton {...props} dense {label}>
			<IconMenu />
		</IconButton>
	{/snippet}
</Menu>
