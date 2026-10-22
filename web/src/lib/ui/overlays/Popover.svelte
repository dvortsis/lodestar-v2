<script lang="ts">
	// Non-modal popover: opens from its trigger, Esc or an outside click closes it. It does not trap
	// focus (§10.13: only Dialog and the modal Sheet do). The panel is portalled to the end of
	// <body>, so Tab from its last control would leave the page: Tab and Shift+Tab are routed back
	// to the page order around the trigger (WCAG 2.4.3).
	import { Popover } from 'bits-ui';
	import type { Snippet } from 'svelte';
	import './overlays.css';

	const TABBABLE =
		'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

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

	let triggerEl = $state<HTMLElement | null>(null);

	// Closing from inside the panel (Esc) unmounts the focused element: hand focus back to the
	// trigger. If focus already moved somewhere real (a click elsewhere), leave it.
	let wasOpen = false;
	$effect(() => {
		const isOpen = open;
		if (wasOpen && !isOpen) {
			queueMicrotask(() => {
				const a = document.activeElement;
				if (!a || a === document.body || a.closest('[data-popover-content]')) triggerEl?.focus();
			});
		}
		wasOpen = isOpen;
	});

	/** Opening moves focus to the panel's first control (or the panel), so the keyboard follows it. */
	function focusOnMount(node: HTMLElement) {
		const panel = node.closest<HTMLElement>('[data-popover-content]');
		(node.querySelector<HTMLElement>(TABBABLE) ?? panel)?.focus({ preventScroll: true });
	}

	function onContentKeydown(event: KeyboardEvent) {
		if (event.key !== 'Tab' || event.defaultPrevented || !triggerEl) return;
		const panel = event.currentTarget as HTMLElement;
		const inside = [...panel.querySelectorAll<HTMLElement>(TABBABLE)];
		const active = document.activeElement;
		if (event.shiftKey) {
			if (active === panel || active === inside[0]) {
				event.preventDefault();
				triggerEl.focus();
			}
		} else if (active === inside.at(-1) || (active === panel && !inside.length)) {
			const after = [...document.querySelectorAll<HTMLElement>(TABBABLE)].find(
				(el) =>
					!panel.contains(el) &&
					!!(triggerEl!.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING)
			);
			if (after) {
				event.preventDefault();
				after.focus();
			}
		}
	}
</script>

<Popover.Root bind:open {onOpenChange}>
	<Popover.Trigger bind:ref={triggerEl}>
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
			onkeydown={onContentKeydown}
			onOpenAutoFocus={(e) => e.preventDefault()}
			onCloseAutoFocus={(e) => e.preventDefault()}
			aria-label={label}
			class="ls-pop z-50 rounded-box border border-base-300 bg-base-100 p-3 text-body text-base-content"
		>
			<div use:focusOnMount>{@render children()}</div>
		</Popover.Content>
	</Popover.Portal>
</Popover.Root>
