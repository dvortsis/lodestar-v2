<script lang="ts">
	// The §10.5 secondary actions in one menu (loaded on the first press, so Bits UI stays out of the
	// first view). Favorite, Hide, Labels and Fix parse join when their doors exist.
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import IconDots from '~icons/ph/dots-three-bold';
	import IconFull from '~icons/ph/arrow-square-out-bold';
	import IconBlock from '~icons/ph/prohibit-bold';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Menu, { type MenuItem } from '$lib/ui/overlays/Menu.svelte';

	interface Props {
		open: boolean;
		mode: 'drawer' | 'page';
		hash: string;
		onblock: () => void;
	}
	let { open = $bindable(), mode, hash, onblock }: Props = $props();

	const items = $derived<MenuItem[]>([
		...(mode === 'drawer' ? [{ id: 'full', label: m.detail_open_full(), icon: IconFull }] : []),
		{
			id: 'block',
			label: m.detail_block(),
			icon: IconBlock,
			separatorBefore: mode === 'drawer'
		}
	]);
	function select(id: string) {
		if (id === 'full') void goto(resolve('/t/[hash]', { hash }));
		else if (id === 'block') onblock();
	}
</script>

<Menu {items} onSelect={select} label={m.detail_actions_menu()} bind:open autofocusFirst>
	{#snippet trigger({ props })}
		<Button {...props} dense aria-label={m.detail_actions_menu()} data-testid="detail-actions-menu">
			<IconDots class="size-(--icon-size-nav)" aria-hidden="true" />
		</Button>
	{/snippet}
</Menu>
