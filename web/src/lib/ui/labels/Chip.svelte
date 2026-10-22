<script lang="ts">
	import { tick, type Snippet } from 'svelte';
	import IconClose from '~icons/ph/x';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '../classes';

	interface Props {
		children: Snippet;
		/** Makes the chip removable; called by the button, Backspace and Delete. */
		onremove?: () => void;
		/** Accessible name of the remove button, e.g. "Remove filter: video". */
		removeLabel?: string;
		/**
		 * Focus after removal: the next removable chip in this chip's group (`data-chip-group`, a
		 * list/group element, or the parent), else the previous one, else this element (e.g. the query
		 * input). Without it focus would drop to <body>.
		 */
		fallbackFocus?: () => HTMLElement | null | undefined;
		/** Selected/active filter tint. */
		selected?: boolean;
		class?: string;
	}

	let {
		children,
		onremove,
		removeLabel,
		fallbackFocus,
		selected = false,
		class: className = ''
	}: Props = $props();

	const GROUP = '[data-chip-group], [role="list"], [role="group"], ul, ol';

	let root: HTMLSpanElement | undefined = $state();

	async function remove() {
		if (!onremove) return;
		// Pick the neighbour before the DOM changes; it is another chip, so it survives the removal.
		// Search only this chip's own group: mark it with data-chip-group, or use a list/group
		// element; a bare container falls back to the chip's parent. Chips may be wrapped in <li>s.
		const scope = root?.closest(GROUP) ?? root?.parentElement;
		const siblings = [...(scope?.querySelectorAll('[data-chip-remove]') ?? [])];
		const own = root?.querySelector('[data-chip-remove]') ?? null;
		const at = siblings.indexOf(own as Element);
		const neighbour = (siblings[at + 1] ?? siblings[at - 1]) as HTMLElement | undefined;
		onremove();
		await tick();
		const target = [neighbour, fallbackFocus?.()].find((el) => el?.isConnected);
		target?.focus();
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Backspace' || event.key === 'Delete') {
			event.preventDefault();
			void remove();
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
			data-chip-remove
			aria-label={removeLabel ?? m.common_remove()}
			class="inline-flex size-6 items-center justify-center rounded-selector hover:bg-hover {FOCUS_RING} {TARGET_MIN}"
			onclick={remove}
			{onkeydown}
		>
			<IconClose class="size-4" aria-hidden="true" />
		</button>
	{/if}
</span>
