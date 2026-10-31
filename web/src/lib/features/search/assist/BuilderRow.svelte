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
	import { FIELDS, OPERATORS, kindOf, type Op, type Row } from './builder';

	interface Props {
		row: Row;
		/** A new row: shows an "Add filter" button. */
		draft?: boolean;
		oncommit: (row: Row) => void;
		onremove?: () => void;
		/** The group's name (the clause's label). */
		label: string;
		removeLabel?: string;
	}
	let { row, draft = false, oncommit, onremove, label, removeLabel }: Props = $props();

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

	const ops = $derived(OPERATORS[kindOf(field)]);
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
	}
	function commit() {
		oncommit({ field, op: op as Op, value });
	}
</script>

<div class="assist-row" role="group" aria-label={label}>
	<label class="assist-field"
		>{m.search_assist_builder_field()}
		<Select
			dense
			bind:value={field}
			onchange={fieldChanged}
			options={FIELDS.map((f) => ({ value: f.name, label: f.name }))}
		/></label
	>
	<label class="assist-field"
		>{m.search_assist_builder_operator()}
		<Select
			dense
			bind:value={op}
			options={ops.map((o) => ({ value: o, label: OP_LABEL[o]() }))}
		/></label
	>
	<label class="assist-field assist-grow"
		>{m.search_assist_builder_value()}
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
		/></label
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
