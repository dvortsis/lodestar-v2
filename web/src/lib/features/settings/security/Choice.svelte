<script lang="ts">
	// A checkbox, switch or radio with its label, styled as `$lib/ui/fields`' Checkbox, Toggle and
	// Radio. Built here because importing those from Security's on-demand parts splits them out of
	// the engine's controls chunk, which adds a preload entry to every Settings page (decisions file).
	import type { HTMLInputAttributes } from 'svelte/elements';
	import { FOCUS_RING } from '$lib/ui/classes';

	interface Props extends Omit<HTMLInputAttributes, 'type' | 'checked' | 'group' | 'children'> {
		kind: 'checkbox' | 'switch' | 'radio';
		label: string;
		checked?: boolean;
		group?: string;
	}
	let { kind, label, checked = $bindable(false), group = $bindable(), ...rest }: Props = $props();
</script>

<label
	class="inline-flex cursor-pointer items-center gap-2 text-body pointer-coarse:min-h-(--target-min)"
>
	{#if kind === 'radio'}
		<input type="radio" bind:group class="radio border-field-border {FOCUS_RING}" {...rest} />
	{:else}
		<input
			type="checkbox"
			role={kind === 'switch' ? 'switch' : undefined}
			bind:checked
			class="{kind === 'switch' ? 'toggle' : 'checkbox'} border-field-border {FOCUS_RING}"
			{...rest}
		/>
	{/if}
	<span>{label}</span>
</label>
