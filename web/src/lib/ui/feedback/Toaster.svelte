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

	// Timers pause while the pointer or focus is inside the toast (WCAG 2.2.1).
	function timed(node: HTMLElement, item: ToastItem) {
		let timer: ReturnType<typeof setTimeout> | undefined;
		const start = () => {
			if (item.timeout > 0) timer = setTimeout(() => toast.dismiss(item.id), item.timeout);
		};
		const stop = () => clearTimeout(timer);
		start();
		node.addEventListener('pointerenter', stop);
		node.addEventListener('pointerleave', start);
		node.addEventListener('focusin', stop);
		node.addEventListener('focusout', start);
		return { destroy: stop };
	}
</script>

<!-- The live region is always in the DOM so screen readers register it before content arrives.
     Toasts never take focus. -->
<div
	role="region"
	aria-label={label ?? m.common_notifications()}
	aria-live="polite"
	aria-relevant="additions text"
	class="pointer-events-none fixed right-4 bottom-4 z-50 flex w-full max-w-(--drawer-overlay-width-narrow) flex-col gap-2"
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
	.toast-in {
		animation: toast-in var(--duration-banner) var(--ease-out);
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
