<script lang="ts">
	// What a field's preview says (§10.10.5): its estimates and warnings under the field, and with
	// Automatic on, the effective value beside the switch. Part of the preview chunk
	// (`preview.ts`), which every preview has loaded.
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import IconWarning from '~icons/ph/warning';
	import { automaticValue, estimatePart } from './display';
	import type { FieldState } from './types';

	interface Props {
		field: FieldState;
		/** Automatic is on: its one estimate shows beside the switch instead. */
		automatic: boolean;
		part: 'automatic' | 'lines';
	}

	let { field: f, automatic, part }: Props = $props();

	const effective = $derived(automatic ? automaticValue(f) : undefined);
	const estimates = $derived(
		effective
			? []
			: (f.preview?.estimates ?? []).flatMap((e) => {
					const p = estimatePart(e);
					return p ? [p] : [];
				})
	);
	const warnings = $derived(f.preview?.warnings ?? []);
</script>

{#if part === 'automatic'}
	{#if effective}
		<span class="text-small text-muted-text" data-testid="automatic-value">
			{m.settings_form_automatic_value({ value: effective })}
		</span>
	{/if}
{:else if estimates.length || warnings.length}
	<p class="flex flex-wrap items-center gap-x-4 gap-y-1 text-small text-muted-text">
		{#each estimates as p (p.testid)}
			<span data-testid={p.testid}>
				{p.text}
				{#if p.mono}<Mono>{p.mono}</Mono>{/if}
			</span>
		{/each}
		{#each warnings as w (w.code)}
			<span class="inline-flex items-center gap-1 text-warning">
				<IconWarning class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				<span class="text-base-content">{w.message}</span>
			</span>
		{/each}
	</p>
{/if}
