<script lang="ts">
	// Hover and focus open it, Esc closes it (Bits UI). Never the only home of essential content
	// (§10.13): the trigger keeps its own accessible name.
	import { Tooltip } from 'bits-ui';
	import type { Snippet } from 'svelte';
	import './overlays.css';

	interface Props {
		content: string | Snippet;
		/** Renders the trigger element; spread `props` onto it. */
		trigger: Snippet<[{ props: Record<string, unknown> }]>;
		side?: 'top' | 'right' | 'bottom' | 'left';
		delayDuration?: number;
		open?: boolean;
	}
	let {
		content,
		trigger,
		side = 'top',
		delayDuration = 300,
		open = $bindable(false)
	}: Props = $props();
</script>

<Tooltip.Provider {delayDuration}>
	<Tooltip.Root bind:open>
		<Tooltip.Trigger>
			{#snippet child({ props })}
				{@render trigger({ props })}
			{/snippet}
		</Tooltip.Trigger>
		<Tooltip.Portal>
			<Tooltip.Content
				{side}
				sideOffset={4}
				role="tooltip"
				class="ls-pop z-50 max-w-xs rounded-field border border-base-300 bg-base-100 px-2 py-1 text-small text-base-content"
			>
				{#if typeof content === 'string'}{content}{:else}{@render content()}{/if}
			</Tooltip.Content>
		</Tooltip.Portal>
	</Tooltip.Root>
</Tooltip.Provider>
