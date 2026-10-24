<script lang="ts">
	// An enum's control (§10.10.4): a segmented control for a few values, a radio group when the
	// schema asks for one, a select for more. Loaded per type by `controls.svelte.ts`.
	import Radio from '$lib/ui/fields/Radio.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import type { LazyControlProps } from './controls.svelte';
	import { optionLabel } from './display';

	let { form, key, builtin, controlProps, label, dense = false }: LazyControlProps = $props();

	const f = $derived(form.field(key));
	const spec = $derived(f.spec);
	const options = $derived(
		(spec.enum ?? []).map((v) => ({ value: v, label: optionLabel(spec, v), disabled: f.locked }))
	);
	const value = $derived(typeof f.value === 'string' ? f.value : undefined);
	const set = (v: string) => form.set(key, v);
	const blur = () => form.blur(key);
</script>

{#if builtin === 'segmented'}
	<!-- SegmentedControl takes no aria-describedby (a ui/ follow-up): the group carries help and
	     error for it. -->
	<div
		role="group"
		aria-labelledby={controlProps['aria-labelledby']}
		aria-describedby={controlProps['aria-describedby']}
	>
		<SegmentedControl {label} {dense} {options} value={value ?? ''} onchange={set} />
	</div>
{:else if builtin === 'radio'}
	<div
		role="radiogroup"
		aria-labelledby={controlProps['aria-labelledby']}
		aria-describedby={controlProps['aria-describedby']}
		aria-invalid={controlProps['aria-invalid']}
		class="flex flex-col gap-1"
	>
		{#each options as o (o.value)}
			<Radio
				name={key}
				value={o.value}
				label={o.label}
				group={value}
				disabled={f.locked}
				onchange={() => set(o.value)}
				onblur={blur}
			/>
		{/each}
	</div>
{:else}
	<Select
		{...controlProps}
		{dense}
		{options}
		{value}
		disabled={f.locked}
		onchange={(e) => set(e.currentTarget.value)}
		onblur={blur}
	/>
{/if}
