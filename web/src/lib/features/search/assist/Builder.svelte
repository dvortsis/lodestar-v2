<script lang="ts">
	// The Builder (§10.4): the query as rows of field / operator / value, round-tripping with the bar
	// through the server's AST. A row's edit is parsed by the server (`/parse`, which owns syntax and
	// units), swapped into the AST and sent back through `setAst`, so the bar shows the server's
	// canonical text; typing in the bar changes the AST and so the rows. Clauses the Builder can't
	// show as one row stay as read-only rows with their label, and can still be removed.
	import { api, ApiError } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import type { Ast } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { chipClauses, rootWithout, topLevel } from '../query/clauses';
	import BuilderRow from './BuilderRow.svelte';
	import { rowOf, rowText, withNode, type Row } from './builder';

	interface Props {
		/** The AST the results show. */
		ast: Ast | null;
		/** Sends the edited AST (W5 `setAst`); the bar's text follows. */
		apply: (ast: Ast) => Promise<void>;
		close: () => void;
		announce: (text: string) => void;
	}
	let { ast, apply, close, announce }: Props = $props();

	const blank: Row = { field: 'res', op: ':', value: '' };
	const clauses = $derived(chipClauses(ast));
	let error = $state('');
	/** Bumped after an add, so the draft row is a fresh, empty one. */
	let draftKey = $state(0);

	async function commit(index: number | null, row: Row) {
		error = '';
		if (!row.value.trim()) {
			error = m.search_assist_builder_empty();
			return;
		}
		try {
			const parsed = await api.get('/parse', { query: { q: rowText(row) } });
			if (parsed.ast.op === 'all') {
				error = m.search_assist_builder_empty();
				return;
			}
			await apply(withNode(ast, index, parsed.ast));
			if (index === null) draftKey++;
			announce(m.search_assist_builder_applied({ text: rowText(row) }));
		} catch (e) {
			error =
				e instanceof ApiError && e.code === 'query_syntax'
					? e.message
					: m.search_assist_builder_failed();
		}
	}

	async function remove(index: number, label: string) {
		if (!ast) return;
		error = '';
		try {
			await apply(rootWithout(ast, index));
			announce(m.search_filters_removed({ label }));
		} catch {
			error = m.search_assist_builder_failed();
		}
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		e.preventDefault();
		close();
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="assist-panel" aria-label={m.search_assist_builder_title()} {onkeydown}>
	{#each clauses as clause (clause.node)}
		{@const index = topLevel(ast).indexOf(clause.node)}
		{@const row = rowOf(clause.node)}
		{@const removeLabel = m.search_filters_remove({ label: clause.label.text })}
		{#if row}
			<BuilderRow
				{row}
				{removeLabel}
				oncommit={(r) => void commit(index, r)}
				onremove={() => void remove(index, clause.label.text)}
			/>
		{:else}
			<div class="assist-row" role="group" aria-label={removeLabel}>
				<p class="assist-grow">{clause.label.text}</p>
				<Button
					variant="quiet"
					dense
					aria-label={removeLabel}
					onclick={() => void remove(index, clause.label.text)}>{m.common_remove()}</Button
				>
			</div>
		{/if}
	{/each}
	{#key draftKey}
		<BuilderRow row={blank} draft oncommit={(r) => void commit(null, r)} />
	{/key}
	{#if error}
		<p role="alert" class="assist-note assist-error">{error}</p>
	{/if}
	<div class="assist-row">
		<Button onclick={close}>{m.search_assist_builder_close()}</Button>
	</div>
</section>
