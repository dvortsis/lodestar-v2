<script lang="ts">
	// Non-modal popover: opens from its trigger, Esc or an outside click closes it. It does not trap
	// focus (§10.13: only Dialog and the modal Sheet do).
	import { Popover } from 'bits-ui';
	import type { Snippet } from 'svelte';
	import './overlays.css';

	interface Props {
		/** Accessible name of the popover panel. */
		label: string;
		trigger: Snippet<[{ props: Record<string, unknown> }]>;
		children: Snippet;
		open?: boolean;
		onOpenChange?: (open: boolean) => void;
		side?: 'top' | 'right' | 'bottom' | 'left';
		align?: 'start' | 'center' | 'end';
	}
	let {
		label,
		trigger,
		children,
		open = $bindable(false),
		onOpenChange,
		side = 'bottom',
		align = 'start'
	}: Props = $props();
</script>

<Popover.Root bind:open {onOpenChange}>
	<Popover.Trigger>
		{#snippet child({ props })}
			{@render trigger({ props })}
		{/snippet}
	</Popover.Trigger>
	<Popover.Portal>
		<Popover.Content
			{side}
			{align}
			sideOffset={4}
			trapFocus={false}
			role="dialog"
			aria-label={label}
			class="ls-pop z-50 rounded-box border border-base-300 bg-base-100 p-3 text-body text-base-content"
		>
			{@render children()}
		</Popover.Content>
	</Popover.Portal>
</Popover.Root>
