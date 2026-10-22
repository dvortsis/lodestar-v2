<script lang="ts">
	// The one modal shell behind Dialog (centred) and Sheet (edge-attached). Focus is trapped while
	// open and restored to the invoker on close (§10.13). Not for direct use by features.
	import { Dialog } from 'bits-ui';
	import type { Snippet } from 'svelte';
	import IconX from '~icons/ph/x';
	import './overlays.css';

	interface Props {
		/** The visible, labelled title (the dialog's accessible name). */
		title: string;
		closeLabel: string;
		description?: string;
		children: Snippet;
		footer?: Snippet;
		trigger?: Snippet<[{ props: Record<string, unknown> }]>;
		open?: boolean;
		onOpenChange?: (open: boolean) => void;
		placement: 'center' | 'left' | 'right' | 'bottom';
		/** Pass `false` to render the title for assistive tech only. */
		showTitle?: boolean;
	}
	let {
		title,
		closeLabel,
		description,
		children,
		footer,
		trigger,
		open = $bindable(false),
		onOpenChange,
		placement,
		showTitle = true
	}: Props = $props();

	// Bits restores focus to the trigger; with no trigger (opened from code) restore to whatever
	// held focus when the modal opened.
	let invoker: Element | null = null;
	$effect(() => {
		if (open) invoker = document.activeElement;
	});
	function restoreFocus(event: Event) {
		if (trigger) return;
		event.preventDefault();
		if (invoker instanceof HTMLElement && invoker.isConnected) invoker.focus();
	}

	const panel: Record<Props['placement'], string> = {
		center:
			'ls-dialog fixed top-1/2 left-1/2 max-h-4/5 w-full max-w-lg -translate-x-1/2 -translate-y-1/2 rounded-sheet',
		right: 'ls-sheet fixed inset-y-0 right-0 w-full max-w-(--drawer-overlay-width) rounded-s-sheet',
		left: 'ls-sheet fixed inset-y-0 left-0 w-full max-w-(--drawer-overlay-width-narrow) rounded-e-sheet',
		bottom: 'ls-sheet fixed inset-x-0 bottom-0 max-h-4/5 rounded-t-sheet'
	};
</script>

<Dialog.Root bind:open {onOpenChange}>
	{#if trigger}
		<Dialog.Trigger>
			{#snippet child({ props })}
				{@render trigger({ props })}
			{/snippet}
		</Dialog.Trigger>
	{/if}
	<Dialog.Portal>
		<Dialog.Overlay class="ls-overlay fixed inset-0 z-40 bg-base-content/40" />
		<Dialog.Content
			onCloseAutoFocus={restoreFocus}
			data-side={placement === 'center' ? undefined : placement}
			class="{panel[
				placement
			]} z-50 flex flex-col overflow-hidden border border-base-300 bg-base-100 text-body text-base-content"
		>
			<div class="flex items-start justify-between gap-3 p-4">
				<Dialog.Title class={showTitle ? 'text-title font-semibold' : 'sr-only'}>
					{title}
				</Dialog.Title>
				<Dialog.Close
					aria-label={closeLabel}
					class="ls-focus inline-flex size-(--control-height-dense) shrink-0 items-center justify-center rounded-field hover:bg-hover pointer-coarse:size-(--target-min)"
				>
					<IconX class="size-(--icon-size-nav)" aria-hidden="true" />
				</Dialog.Close>
			</div>
			{#if description}
				<Dialog.Description class="px-4 pb-2 text-muted-text">{description}</Dialog.Description>
			{/if}
			<div class="min-h-0 flex-1 overflow-y-auto px-4 pb-4">{@render children()}</div>
			{#if footer}
				<div class="flex justify-end gap-2 border-t border-base-300 p-4">{@render footer()}</div>
			{/if}
		</Dialog.Content>
	</Dialog.Portal>
</Dialog.Root>
