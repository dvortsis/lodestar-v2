<script lang="ts">
	// A number with its unit as a suffix inside the control (§10.10.4). What's typed stays as
	// typed: text that isn't a number is staged as is, so validation can say so.
	import Input from '$lib/ui/fields/Input.svelte';
	import type { SettingsForm } from './form.svelte';
	import { unitText } from './messages';
	import type { FieldKey } from './types';
	import type { FieldControlProps } from './widgets';

	interface Props {
		form: SettingsForm;
		key: FieldKey;
		controlProps: FieldControlProps;
		dense?: boolean;
	}

	let { form, key, controlProps, dense = false }: Props = $props();

	const f = $derived(form.field(key));
	const unit = $derived(unitText(f.spec.unit));
	const unitId = $props.id();
	const shown = (v: unknown) =>
		typeof v === 'number' ? String(v) : typeof v === 'string' ? v : '';

	let text = $state('');
	// Follow the form when it changes the value (discard, merge, Use theirs), not while typing.
	$effect(() => {
		const v = f.value;
		if (parse(text) !== v) text = shown(v);
	});

	function parse(s: string): unknown {
		const t = s.trim();
		if (t === '') return '';
		const n = Number(t);
		return Number.isFinite(n) ? n : s;
	}
</script>

<div class="ls-number relative">
	<Input
		{...controlProps}
		{dense}
		type="text"
		inputmode={f.spec.kind === 'integer' ? 'numeric' : 'decimal'}
		autocomplete="off"
		value={text}
		disabled={f.locked}
		aria-describedby={[controlProps['aria-describedby'], unit ? unitId : '']
			.filter(Boolean)
			.join(' ') || undefined}
		oninput={(e) => {
			text = e.currentTarget.value;
			form.set(key, parse(text));
		}}
		onblur={() => form.blur(key)}
	/>
	{#if unit}
		<span id={unitId} class="ls-unit text-small text-muted-text">{unit}</span>
	{/if}
</div>

<style>
	.ls-number {
		max-width: calc(var(--spacing) * 64);
	}
	.ls-number :global(input) {
		padding-inline-end: calc(var(--spacing) * 24);
	}
	.ls-unit {
		position: absolute;
		inset-block: 0;
		inset-inline-end: calc(var(--spacing) * 3);
		display: flex;
		align-items: center;
		pointer-events: none;
	}
</style>
