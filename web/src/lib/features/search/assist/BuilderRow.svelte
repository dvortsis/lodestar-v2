<script lang="ts">
	// One Builder row: field / operator / value. An existing row shows "Apply" once edited (and
	// Enter in the value applies); the draft row at the end has "Add filter". Changing a select alone
	// never commits, so an arrow key on a closed select can't rewrite the query. The row only reports
	// text; the server parses it.
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import type { Row } from './builder';
	import { opsOf, type Field, type Op } from './fields.svelte';

	interface Props {
		row: Row;
		/** The server's fields (`/search/fields`): all the row may name. */
		fields: readonly Field[];
		/** A new row: shows an "Add filter" button. */
		draft?: boolean;
		/** Show the Field / Operator / Value captions (first row only; the rest keep them as screen-reader names). */
		captions?: boolean;
		oncommit: (row: Row) => void;
		onremove?: () => void;
		/** The group's name (the clause's label). */
		label: string;
		removeLabel?: string;
	}
	let {
		row,
		fields,
		draft = false,
		captions = true,
		oncommit,
		onremove,
		label,
		removeLabel
	}: Props = $props();

	let field = $state(untrack(() => row.field));
	let op = $state<string>(untrack(() => row.op));
	let value = $state(untrack(() => row.value));

	// The clause changed under the row (typing in the bar, a commit): show it, keeping focus where
	// it is, since rows are keyed by position.
	$effect(() => {
		const r = row;
		untrack(() => {
			field = r.field;
			op = r.op;
			value = r.value;
		});
	});

	const spec = $derived(fields.find((f) => f.name === field));
	const ops = $derived(opsOf(spec));
	const opt = (value: string) => ({ value, label: value });
	// A fixed list is a Select only for one listed value; several (`movie,tv`) or a share's
	// `video>=50%` stay text.
	const valueOptions = $derived(
		spec?.values && spec.type !== 'share' && (!value || spec.values.includes(value))
			? spec.values.map(opt)
			: undefined
	);
	const dirty = $derived(field !== row.field || op !== row.op || value !== row.value);
	const OP_LABEL: Record<Op, () => string> = {
		':': m.search_assist_op_is,
		'>=': m.search_assist_op_gte,
		'<=': m.search_assist_op_lte,
		'>': m.search_assist_op_gt,
		'<': m.search_assist_op_lt
	};

	function fieldChanged() {
		// A field with other operators: the first one that fits.
		if (!ops.includes(op as Op)) op = ops[0]!;
		if (valueOptions) value = '';
	}
	function commit() {
		oncommit({ field, op: op as Op, value });
	}
</script>

<div class="assist-row" role="group" aria-label={label}>
	<label class="assist-field"
		><span class:sr-only={!captions}>{m.search_assist_builder_field()}</span>
		<Select
			dense
			bind:value={field}
			onchange={fieldChanged}
			options={fields.map((f) => opt(f.name))}
		/></label
	>
	<label class="assist-field"
		><span class:sr-only={!captions}>{m.search_assist_builder_operator()}</span>
		<Select
			dense
			bind:value={op}
			options={ops.map((o) => ({ value: o, label: OP_LABEL[o]() }))}
		/></label
	>
	<label class="assist-field assist-grow"
		><span class:sr-only={!captions}>{m.search_assist_builder_value()}</span>
		{#if valueOptions}
			<Select dense bind:value options={valueOptions} placeholder="" />
		{:else}
			<Input
				dense
				mono
				bind:value
				onkeydown={(e) => {
					if (e.key === 'Enter' && (draft || dirty)) {
						e.preventDefault();
						commit();
					}
				}}
			/>
		{/if}</label
	>
	{#if draft}
		<Button variant="primary" dense onclick={commit}>{m.search_assist_builder_add()}</Button>
	{:else}
		{#if dirty}
			<Button variant="primary" dense onclick={commit}>{m.search_assist_builder_apply()}</Button>
		{/if}
		{#if onremove}
			<Button variant="quiet" dense aria-label={removeLabel} onclick={onremove}
				>{m.common_remove()}</Button
			>
		{/if}
	{/if}
</div>
