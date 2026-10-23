<script lang="ts">
	// Modal dialog: focus trap, focus restored on close, Esc closes, a labelled title.
	import Modal from './Modal.svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		closeLabel: string;
		description?: string;
		children: Snippet;
		footer?: Snippet;
		trigger?: Snippet<[{ props: Record<string, unknown> }]>;
		open?: boolean;
		onOpenChange?: (open: boolean) => void;
		/** Where focus starts on open (e.g. Cancel, §10.10.7); default: the first focusable element. */
		initialFocus?: () => HTMLElement | null | undefined;
	}
	let { open = $bindable(false), ...rest }: Props = $props();
</script>

<Modal bind:open placement="center" {...rest} />
