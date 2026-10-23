<script lang="ts">
	// The built-in controls of §10.10.4's table, by the name `widgetFor` picked. Each reads the
	// field from the form and writes back through `form.set`; validation runs on blur.
	import Input from '$lib/ui/fields/Input.svelte';
	import Radio from '$lib/ui/fields/Radio.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Slider from '$lib/ui/fields/Slider.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import ChipsControl from './ChipsControl.svelte';
	import JsonControl from './JsonControl.svelte';
	import NumberControl from './NumberControl.svelte';
	import { optionLabel } from './display';
	import type { SettingsForm } from './form.svelte';
	import type { FieldKey } from './types';
	import type { BuiltinWidget, FieldControlProps } from './widgets';

	interface Props {
		form: SettingsForm;
		key: FieldKey;
		widget: BuiltinWidget;
		mono: boolean;
		controlProps: FieldControlProps;
		/** The field's label, for controls that carry their own (toggle, segmented, radio). */
		label: string;
		dense?: boolean;
	}

	let { form, key, widget, mono, controlProps, label, dense = false }: Props = $props();

	const f = $derived(form.field(key));
	const spec = $derived(f.spec);
	const options = $derived(
		(spec.enum ?? []).map((v) => ({ value: v, label: optionLabel(spec, v), disabled: f.locked }))
	);
	const set = (v: unknown) => form.set(key, v);
	const blur = () => form.blur(key);
</script>

{#if widget === 'toggle'}
	<Toggle
		{label}
		checked={f.value === true}
		disabled={f.locked}
		{...controlProps}
		onchange={(e) => set(e.currentTarget.checked)}
		onblur={blur}
	/>
{:else if widget === 'segmented'}
	<!-- SegmentedControl takes no aria-describedby (a ui/ follow-up): the group carries help and
	     error for it. -->
	<div
		role="group"
		aria-labelledby={controlProps['aria-labelledby']}
		aria-describedby={controlProps['aria-describedby']}
	>
		<SegmentedControl
			{label}
			{dense}
			{options}
			value={typeof f.value === 'string' ? f.value : ''}
			onchange={set}
		/>
	</div>
{:else if widget === 'radio'}
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
				group={typeof f.value === 'string' ? f.value : undefined}
				disabled={f.locked}
				onchange={() => set(o.value)}
				onblur={blur}
			/>
		{/each}
	</div>
{:else if widget === 'select'}
	<Select
		{...controlProps}
		{dense}
		{options}
		value={typeof f.value === 'string' ? f.value : undefined}
		disabled={f.locked}
		onchange={(e) => set(e.currentTarget.value)}
		onblur={blur}
	/>
{:else if widget === 'number'}
	<NumberControl {form} {key} {controlProps} {dense} />
{:else if widget === 'slider'}
	<div class="flex items-center gap-3">
		<Slider
			value={typeof f.value === 'number' ? f.value : (spec.minimum ?? 0)}
			min={spec.minimum}
			max={spec.maximum}
			disabled={f.locked}
			aria-labelledby={controlProps['aria-labelledby']}
			oninput={(e) => set(Number(e.currentTarget.value))}
			onblur={blur}
		/>
		<NumberControl {form} {key} {controlProps} {dense} />
	</div>
{:else if widget === 'text'}
	<Input
		{...controlProps}
		{dense}
		{mono}
		value={typeof f.value === 'string' ? f.value : ''}
		disabled={f.locked}
		spellcheck={mono ? false : undefined}
		autocomplete="off"
		oninput={(e) => set(e.currentTarget.value)}
		onblur={blur}
	/>
{:else if widget === 'chips'}
	<ChipsControl {form} {key} {controlProps} {dense} />
{:else}
	<JsonControl {form} {key} {controlProps} />
{/if}
