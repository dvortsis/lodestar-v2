<script lang="ts">
	// Non-modal detail drawer (§10.5): it keeps the list visible and does NOT trap focus. Focus moves
	// to the title on open; Esc (with focus inside) closes it and returns focus to the invoking row.
	// The width is controlled: the feature persists it from `onWidthChange` (no storage in here).
	import type { Snippet } from 'svelte';
	import { drawerSlide } from './motion';
	import IconX from '~icons/ph/x';
	import './overlays.css';

	interface Props {
		open?: boolean;
		title: string;
		closeLabel: string;
		/** Accessible name of the resize handle. */
		resizeLabel: string;
		children: Snippet;
		/** Actions rendered beside the title (primary action, menu). */
		actions?: Snippet;
		/** Width in px; clamped to [minWidth, maxWidth]. */
		width?: number;
		minWidth?: number;
		maxWidth?: number;
		onWidthChange?: (width: number) => void;
		onClose?: () => void;
		/** Where focus goes on close. Defaults to the element focused when the drawer opened. */
		returnFocus?: () => HTMLElement | null | undefined;
	}
	let {
		open = $bindable(false),
		title,
		closeLabel,
		resizeLabel,
		children,
		actions,
		width = 560,
		minWidth = 480,
		maxWidth = 640,
		onWidthChange,
		onClose,
		returnFocus
	}: Props = $props();

	const STEP = 16;
	let titleEl = $state<HTMLElement>();
	let invoker: Element | null = null;
	let dragWidth = $state<number | null>(null);
	const clampWidth = (w: number) => Math.min(maxWidth, Math.max(minWidth, Math.round(w)));
	const shown = $derived(clampWidth(dragWidth ?? width));
	const titleId = $props.id();

	$effect(() => {
		if (open) {
			invoker = document.activeElement;
			titleEl?.focus();
		}
	});

	function close() {
		const target = returnFocus?.() ?? invoker;
		open = false;
		onClose?.();
		if (target instanceof HTMLElement && target.isConnected) target.focus();
	}

	function onKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape' && !event.defaultPrevented) {
			event.preventDefault();
			close();
		}
	}

	function setWidth(w: number) {
		const next = clampWidth(w);
		if (next !== width) onWidthChange?.(next);
	}

	function onHandleKey(event: KeyboardEvent) {
		// The drawer is on the trailing edge: Left widens it, Right narrows it.
		const step = event.shiftKey ? STEP * 4 : STEP;
		const delta =
			event.key === 'ArrowLeft'
				? step
				: event.key === 'ArrowRight'
					? -step
					: event.key === 'Home'
						? maxWidth
						: event.key === 'End'
							? -maxWidth
							: 0;
		if (!delta) return;
		event.preventDefault();
		event.stopPropagation();
		setWidth(shown + delta);
	}

	function onHandleDown(event: PointerEvent) {
		const handle = event.currentTarget as HTMLElement;
		const startX = event.clientX;
		const startWidth = shown;
		handle.setPointerCapture(event.pointerId);
		const move = (e: PointerEvent) => (dragWidth = clampWidth(startWidth + (startX - e.clientX)));
		const up = () => {
			handle.removeEventListener('pointermove', move);
			handle.removeEventListener('pointerup', up);
			handle.removeEventListener('pointercancel', up);
			const final = dragWidth;
			dragWidth = null;
			if (final !== null) setWidth(final);
		};
		handle.addEventListener('pointermove', move);
		handle.addEventListener('pointerup', up);
		handle.addEventListener('pointercancel', up);
	}
</script>

{#if open}
	<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
	<!-- Esc is handled for focus inside the drawer; features own any list-level shortcuts. -->
	<aside
		aria-labelledby={titleId}
		onkeydown={onKeydown}
		in:drawerSlide
		out:drawerSlide={{ out: true }}
		style:width="{shown}px"
		class="relative flex h-full max-w-full shrink-0 flex-col border-s border-base-300 bg-base-100 text-body text-base-content"
		data-testid="drawer"
	>
		<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
		<div
			role="separator"
			aria-orientation="vertical"
			aria-label={resizeLabel}
			aria-valuemin={minWidth}
			aria-valuemax={maxWidth}
			aria-valuenow={shown}
			tabindex="0"
			onkeydown={onHandleKey}
			onpointerdown={onHandleDown}
			class="ls-focus absolute inset-y-0 start-0 z-10 w-2 -translate-x-1/2 cursor-col-resize touch-none hover:bg-selected pointer-coarse:w-(--target-min)"
		></div>
		<header class="flex items-start justify-between gap-3 border-b border-base-300 p-4">
			<h2
				id={titleId}
				bind:this={titleEl}
				tabindex="-1"
				class="ls-focus min-w-0 text-title font-semibold break-words"
			>
				{title}
			</h2>
			<div class="flex shrink-0 items-center gap-2">
				{#if actions}{@render actions()}{/if}
				<button
					type="button"
					aria-label={closeLabel}
					onclick={close}
					class="ls-focus inline-flex size-(--control-height-dense) items-center justify-center rounded-field hover:bg-hover pointer-coarse:size-(--target-min)"
				>
					<IconX class="size-(--icon-size-nav)" aria-hidden="true" />
				</button>
			</div>
		</header>
		<div class="min-h-0 flex-1 overflow-y-auto p-4">{@render children()}</div>
	</aside>
{/if}
