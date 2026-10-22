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
	}
	let { open = $bindable(false), ...rest }: Props = $props();
</script>

<Modal bind:open placement="center" {...rest} />
