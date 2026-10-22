<script lang="ts">
	import type { Snippet } from 'svelte';
	import IconClose from '~icons/ph/x';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props {
		children: Snippet;
		/** Makes the chip removable; called by the button, Backspace and Delete. */
		onremove?: () => void;
		/** Accessible name of the remove button, e.g. "Remove filter: video". */
		removeLabel?: string;
		/** Selected/active filter tint. */
		selected?: boolean;
		class?: string;
	}

	let {
		children,
		onremove,
		removeLabel,
		selected = false,
		class: className = ''
	}: Props = $props();

	let root: HTMLSpanElement | undefined = $state();

	function onkeydown(event: KeyboardEvent) {
		if (!onremove) return;
		if (event.key === 'Backspace' || event.key === 'Delete') {
			event.preventDefault();
			onremove();
		}
	}
</script>

<span
	bind:this={root}
	class="inline-flex items-center gap-1 rounded-selector border pl-2 text-small {selected
		? 'border-primary bg-selected text-base-content'
		: 'border-field-border bg-base-100 text-base-content'} {onremove ? '' : 'pr-2'} {className}"
>
	<span>{@render children()}</span>
	{#if onremove}
		<button
			type="button"
			aria-label={removeLabel ?? m.common_remove()}
			class="inline-flex size-6 items-center justify-center rounded-selector hover:bg-hover {FOCUS_RING} {TARGET_MIN}"
			onclick={onremove}
			{onkeydown}
		>
			<IconClose class="size-4" aria-hidden="true" />
		</button>
	{/if}
</span>
