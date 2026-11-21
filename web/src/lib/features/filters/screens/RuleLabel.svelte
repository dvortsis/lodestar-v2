<script lang="ts">
	// A rule's or pack's plain name with its code beside it as secondary mono text (the code is
	// what the docs and logs name), and what it checks on the line below (I5). A code with no
	// name shows alone, never an empty label.
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { named, nameOf, textOf } from './names.svelte';

	interface Props {
		id: string;
		/** The code as shown, when it carries more than the id (a version: `MW-4@1`). */
		code?: string;
	}
	let { id, code = id }: Props = $props();
	const text = $derived(textOf(id));
</script>

<span class="flex min-w-0 basis-full flex-col gap-1">
	<span class="flex flex-wrap items-baseline gap-2">
		{#if named(id)}<span class="font-semibold" data-testid="rule-name-{id}">{nameOf(id)}</span>{/if}
		<Mono class={named(id) ? 'text-muted-text' : ''}>{code}</Mono>
	</span>
	{#if text}<span class="text-small text-muted-text">{text}</span>{/if}
</span>
