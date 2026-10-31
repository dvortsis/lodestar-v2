<script lang="ts">
	// One Builder row: field / operator / value. An existing row commits when a control changes; the
	// draft row at the end commits with its button. The row only reports text; the server parses it.
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import { FIELDS, OPERATORS, kindOf, type Op, type Row } from './builder';

	interface Props {
		row: Row;
		/** A new row: shows an "Add filter" button instead of committing on change. */
		draft?: boolean;
		oncommit: (row: Row) => void;
		onremove?: () => void;
		removeLabel?: string;
	}
	let { row, draft = false, oncommit, onremove, removeLabel }: Props = $props();

	// The row as it was when it mounted; the Builder keys rows by clause, so a new clause is a new row.
	const start = untrack(() => row);
	let field = $state(start.field);
	let op = $state<string>(start.op);
	let value = $state(start.value);

	const ops = $derived(OPERATORS[kindOf(field)]);
	const OP_LABEL: Record<Op, () => string> = {
		':': m.search_assist_op_is,
		'>=': m.search_assist_op_gte,
		'<=': m.search_assist_op_lte,
		'>': m.search_assist_op_gt,
		'<': m.search_assist_op_lt
	};

	function changed() {
		// A field with other operators: the first one that fits.
		if (!ops.includes(op as Op)) op = ops[0]!;
		if (!draft) oncommit({ field, op: op as Op, value });
	}
	function add() {
		oncommit({ field, op: op as Op, value });
	}
</script>

<div
	class="assist-row"
	role="group"
	aria-label={draft ? m.search_assist_builder_new() : removeLabel}
>
	<label class="assist-field"
		>{m.search_assist_builder_field()}
		<Select
			dense
			bind:value={field}
			onchange={changed}
			options={FIELDS.map((f) => ({ value: f.name, label: f.name }))}
		/></label
	>
	<label class="assist-field"
		>{m.search_assist_builder_operator()}
		<Select
			dense
			bind:value={op}
			onchange={changed}
			options={ops.map((o) => ({ value: o, label: OP_LABEL[o]() }))}
		/></label
	>
	<label class="assist-field assist-grow"
		>{m.search_assist_builder_value()}
		<Input
			dense
			mono
			bind:value
			onchange={draft ? undefined : changed}
			onkeydown={(e) => {
				if (draft && e.key === 'Enter') {
					e.preventDefault();
					add();
				}
			}}
		/></label
	>
	{#if draft}
		<Button variant="primary" dense onclick={add}>{m.search_assist_builder_add()}</Button>
	{:else if onremove}
		<Button variant="quiet" dense aria-label={removeLabel} onclick={onremove}
			>{m.common_remove()}</Button
		>
	{/if}
</div>
