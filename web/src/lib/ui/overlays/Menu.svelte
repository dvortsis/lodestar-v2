<script lang="ts">
	// Menu-button pattern (Bits UI): Enter/Space/ArrowDown opens, arrows and Home/End move,
	// typeahead jumps by label, Esc closes and returns focus to the trigger.
	import { DropdownMenu } from 'bits-ui';
	import type { Component, Snippet } from 'svelte';
	import './overlays.css';

	export interface MenuItem {
		id: string;
		label: string;
		icon?: Component;
		disabled?: boolean;
		/** Destructive action: coloured with the error role, still labelled by text. */
		danger?: boolean;
		/** Renders a separator above this item. */
		separatorBefore?: boolean;
	}

	interface Props {
		items: MenuItem[];
		onSelect: (id: string) => void;
		trigger: Snippet<[{ props: Record<string, unknown> }]>;
		/** Accessible name of the menu. */
		label: string;
		open?: boolean;
		align?: 'start' | 'center' | 'end';
	}
	let { items, onSelect, trigger, label, open = $bindable(false), align = 'end' }: Props = $props();
</script>

<DropdownMenu.Root bind:open>
	<DropdownMenu.Trigger>
		{#snippet child({ props })}
			{@render trigger({ props })}
		{/snippet}
	</DropdownMenu.Trigger>
	<DropdownMenu.Portal>
		<DropdownMenu.Content
			{align}
			sideOffset={4}
			aria-label={label}
			class="ls-pop z-50 min-w-max rounded-box border border-base-300 bg-base-100 p-1 text-body text-base-content"
		>
			{#each items as item (item.id)}
				{#if item.separatorBefore}
					<DropdownMenu.Separator class="my-1 h-px bg-base-300" />
				{/if}
				<DropdownMenu.Item
					disabled={item.disabled}
					onSelect={() => onSelect(item.id)}
					class="ls-menu-item flex min-h-(--control-height-dense) cursor-default items-center gap-2 rounded-selector px-2 text-body outline-none data-highlighted:bg-hover data-disabled:text-muted-text pointer-coarse:min-h-(--target-min) {item.danger
						? 'text-error'
						: ''}"
				>
					{#if item.icon}
						{@const Icon = item.icon}
						<Icon class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
					{/if}
					{item.label}
				</DropdownMenu.Item>
			{/each}
		</DropdownMenu.Content>
	</DropdownMenu.Portal>
</DropdownMenu.Root>
