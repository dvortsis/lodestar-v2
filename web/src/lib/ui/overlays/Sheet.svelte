<script lang="ts">
	// Modal sheet for narrow screens (filters, detail on small viewports): a bottom sheet by default,
	// or attached to the left/right edge. Traps focus like Dialog.
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
		side?: 'bottom' | 'left' | 'right';
	}
	let { open = $bindable(false), side = 'bottom', ...rest }: Props = $props();
</script>

<Modal bind:open placement={side} {...rest} />
