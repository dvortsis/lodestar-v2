<script lang="ts">
	import IconInfo from '~icons/ph/info';
	import IconSuccess from '~icons/ph/check-circle';
	import IconWarning from '~icons/ph/warning';
	import IconError from '~icons/ph/x-circle';
	import IconClose from '~icons/ph/x';
	import { m } from '$lib/paraglide/messages.js';
	import { FOCUS_RING, TARGET_MIN } from '../classes';
	import { toast, toasts, type ToastItem } from './toasts.svelte';

	interface Props {
		/** Region name; defaults to the shared "Notifications". */
		label?: string;
		/** Name of each dismiss button; defaults to the shared "Dismiss". */
		dismissLabel?: string;
	}

	let { label, dismissLabel }: Props = $props();

	const icons = { info: IconInfo, success: IconSuccess, warning: IconWarning, error: IconError };
	const ink = {
		info: 'text-info',
		success: 'text-success',
		warning: 'text-warning',
		error: 'text-error'
	};
	const kindLabel = {
		info: m.common_info,
		success: m.common_success,
		warning: m.common_warning,
		error: m.common_error
	};

	// Timers pause while the pointer is over the toast or focus is inside it (WCAG 2.2.1); the
	// countdown restarts only once both have left.
	function timed(node: HTMLElement, item: ToastItem) {
		let timer: ReturnType<typeof setTimeout> | undefined;
		let hovered = false;
		let focused = false;
		const sync = () => {
			clearTimeout(timer);
			if (!hovered && !focused && item.timeout > 0) {
				timer = setTimeout(() => toast.dismiss(item.id), item.timeout);
			}
		};
		const on = (type: string, fn: () => void) => node.addEventListener(type, fn);
		on('pointerenter', () => ((hovered = true), sync()));
		on('pointerleave', () => ((hovered = false), sync()));
		on('focusin', () => ((focused = true), sync()));
		on('focusout', () => ((focused = false), sync()));
		sync();
		return { destroy: () => clearTimeout(timer) };
	}
</script>

<!-- The live region is always in the DOM so screen readers register it before content arrives.
     Toasts never take focus. -->
<div
	role="region"
	aria-label={label ?? m.common_notifications()}
	aria-live="polite"
	aria-relevant="additions text"
	class="toaster pointer-events-none fixed inset-x-4 bottom-4 z-50 flex flex-col gap-2 large:left-auto"
>
	{#each toasts.items as item (item.id)}
		{@const Icon = icons[item.kind]}
		<div
			use:timed={item}
			class="toast-in pointer-events-auto flex items-start gap-3 rounded-box border border-base-300 bg-base-100 p-3 text-body text-base-content"
			data-testid="toast"
		>
			<Icon class="mt-0.5 size-5 shrink-0 {ink[item.kind]}" aria-hidden="true" />
			<p class="min-w-0 flex-1">
				<span class="sr-only">{kindLabel[item.kind]()}:&nbsp;</span>{item.text}
			</p>
			<button
				type="button"
				aria-label={dismissLabel ?? m.common_dismiss()}
				class="inline-flex size-6 shrink-0 items-center justify-center rounded-selector hover:bg-hover {FOCUS_RING} {TARGET_MIN}"
				onclick={() => toast.dismiss(item.id)}
			>
				<IconClose class="size-4" aria-hidden="true" />
			</button>
		</div>
	{/each}
</div>

<style>
	/* Opacity only; the banner duration token is 0 under reduced motion, so it appears instantly. */
	/* No toast token exists yet: the width is derived from the row height, the fade reuses the
	   banner duration (0 under reduced motion). Listed as a token gap in the W7ab PR. */
	.toaster {
		--toast-width: calc(var(--row-height-comfortable) * 6);
		--toast-duration: var(--duration-banner);
	}
	@media (min-width: 640px) {
		.toaster {
			width: var(--toast-width);
		}
	}
	.toast-in {
		animation: toast-in var(--toast-duration) var(--ease-out);
	}
	@keyframes toast-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}
</style>
