<script lang="ts">
	import { tick } from 'svelte';
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
		/**
		 * Where focus goes after dismissing a toast that held it, when no neighbouring toast is left
		 * and the element focus came from is gone. Defaults to that element.
		 */
		returnFocus?: () => HTMLElement | null | undefined;
	}

	let { label, dismissLabel, returnFocus }: Props = $props();

	let region: HTMLDivElement | undefined = $state();
	// The element focus came from when it entered the region; kept only while focus stays inside.
	let cameFrom: HTMLElement | null = null;
	let focusInside = false;
	// The toast that holds focus, so the effect below only reacts to *its* removal.
	let focusedId: number | null = null;

	function onfocusin(event: FocusEvent) {
		if (!focusInside) {
			const from = event.relatedTarget as HTMLElement | null;
			cameFrom = from && !region?.contains(from) ? from : null;
		}
		focusInside = true;
		const id = (event.target as Element).closest('[data-toast-id]')?.getAttribute('data-toast-id');
		focusedId = id ? Number(id) : null;
	}

	// Clicking elsewhere drops focus without a relatedTarget; that is the user leaving, not a removal.
	function onwindowpointerdown(event: PointerEvent) {
		if (region?.contains(event.target as Node)) return;
		focusInside = false;
		focusedId = null;
		cameFrom = null;
	}

	function onfocusout(event: FocusEvent) {
		// Focus moved to another element outside the region. A null relatedTarget (the focused toast
		// was removed, or the page lost focus) keeps the state so a removal can still restore focus.
		const to = event.relatedTarget as Node | null;
		if (to && !region?.contains(to)) {
			focusInside = false;
			focusedId = null;
			cameFrom = null;
		}
	}

	async function restoreFocus(neighbour?: HTMLElement) {
		await tick();
		const first = region?.querySelector<HTMLElement>('[data-toast-dismiss]');
		const target = [neighbour, first, cameFrom, returnFocus?.()].find((el) => el?.isConnected);
		focusInside = !!target && !!region?.contains(target);
		if (!focusInside) {
			cameFrom = null;
			focusedId = null;
		}
		target?.focus();
	}

	// Dismissing the toast that holds focus must not drop it to <body>: next toast's button, else the
	// previous one, else where focus came from, else `returnFocus`.
	function dismiss(id: number, button: HTMLElement) {
		const held = region?.contains(document.activeElement);
		const buttons = [...(region?.querySelectorAll<HTMLElement>('[data-toast-dismiss]') ?? [])];
		const at = buttons.indexOf(button);
		const neighbour = buttons[at + 1] ?? buttons[at - 1];
		toast.dismiss(id);
		if (held) void restoreFocus(neighbour);
	}

	// The queue cap can drop a toast that holds focus; treat it like a dismissal. A new toast arriving
	// never moves focus: only the removal of the toast that held it does.
	$effect(() => {
		const ids = toasts.items.map((t) => t.id);
		if (!focusInside || focusedId === null || ids.includes(focusedId)) return;
		void tick().then(() => {
			if (focusInside && !region?.contains(document.activeElement)) void restoreFocus();
		});
	});

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

<svelte:window onpointerdown={onwindowpointerdown} />

<!-- The live region is always in the DOM so screen readers register it before content arrives.
     Toasts never take focus. -->
<div
	role="region"
	aria-label={label ?? m.common_notifications()}
	aria-live="polite"
	aria-relevant="additions text"
	bind:this={region}
	{onfocusin}
	{onfocusout}
	class="pointer-events-none fixed inset-x-4 bottom-4 z-50 flex flex-col gap-2 large:left-auto large:w-(--toast-width)"
>
	{#each toasts.items as item (item.id)}
		{@const Icon = icons[item.kind]}
		<div
			use:timed={item}
			class="toast-in pointer-events-auto flex items-start gap-3 rounded-box border border-base-300 bg-base-100 p-3 text-body text-base-content"
			data-testid="toast"
			data-toast-id={item.id}
		>
			<Icon class="mt-0.5 size-5 shrink-0 {ink[item.kind]}" aria-hidden="true" />
			<p class="min-w-0 flex-1">
				<span class="sr-only">{kindLabel[item.kind]()}:&nbsp;</span>{item.text}
			</p>
			<button
				type="button"
				aria-label={dismissLabel ?? m.common_dismiss()}
				class="inline-flex size-6 shrink-0 items-center justify-center rounded-selector hover:bg-hover {FOCUS_RING} {TARGET_MIN}"
				data-toast-dismiss
				onclick={(e) => dismiss(item.id, e.currentTarget)}
			>
				<IconClose class="size-4" aria-hidden="true" />
			</button>
		</div>
	{/each}
</div>

<style>
	/* Opacity only. --duration-banner is 0 under reduced motion, so the toast just appears. */
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
