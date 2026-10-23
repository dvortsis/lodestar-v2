<script lang="ts">
	// The built-in controls of §10.10.4's table, by the name `widgetFor` picked. Each reads the
	// field from the form and writes back through `form.set`; validation runs on blur. A toggle
	// and a text input are here; the rest load per type (`LazyControl`, `controls.svelte.ts`).
	import Input from '$lib/ui/fields/Input.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import LazyControl from './LazyControl.svelte';
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
{:else}
	<LazyControl builtin={widget} {form} {key} {controlProps} {label} {dense} />
{/if}
