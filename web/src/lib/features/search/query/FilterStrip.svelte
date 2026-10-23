<script lang="ts">
	// The active-filter strip (§10.4): one removable chip per structured clause, the `hidden=1` chip
	// and "Clear all". Loaded only once there is a chip. Changes swap in place, with no animation
	// (§10.2: filter changes never re-animate).
	import { tick, untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Chip from '$lib/ui/labels/Chip.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import type { Clause, Label } from './clauses';

	interface Props {
		clauses: Clause[];
		/** `hidden=1` in the URL. */
		hidden: boolean;
		/** The editor: focus goes there when the last chip goes. */
		el: HTMLElement;
		onremove: (clause: Clause) => Promise<void>;
		onremovehidden: (label: string) => Promise<void>;
		onclear: () => Promise<void>;
	}
	let { clauses, hidden, el, onremove, onremovehidden, onclear }: Props = $props();

	const hiddenLabel = $derived(m.search_filters_including({ term: m.hidden_by_filters() }));

	/** The change that failed, to retry. */
	let failed = $state<(() => Promise<void>) | null>(null);

	async function run(change: () => Promise<void>) {
		failed = null;
		try {
			await change();
		} catch {
			failed = change; // the chip stays: nothing changed
			return;
		}
		// The focused chip can go with the change (the last one, or two removed at once): focus
		// the editor rather than leave it on <body>.
		await tick();
		if (!document.activeElement || document.activeElement === document.body) el.focus();
	}

	const count = $derived(clauses.length + (hidden ? 1 : 0));

	// Keyed by the clause's node, not its position or label: removing one chip must not re-create
	// its neighbours (even one with the same label), which would take the focus Chip hands on.
	const ids = new WeakMap<object, number>();
	let nextId = 0;
	const keyOf = (clause: Clause) => {
		let id = ids.get(clause.node);
		if (id === undefined) ids.set(clause.node, (id = nextId++));
		return id;
	};

	// A Retry belongs to the filters it failed on: a new query drops it.
	$effect(() => {
		void clauses;
		untrack(() => (failed = null));
	});
</script>

{#snippet text(label: Label)}
	<span class="sr-only">{label.text}</span>
	<span aria-hidden="true"
		>{#each label.parts as part, i (i)}{#if typeof part === 'string'}{part}{:else}<Mono
					>{part.mono}</Mono
				>{/if}{/each}</span
	>
{/snippet}

<div
	role="group"
	aria-label={m.search_filters_label()}
	data-chip-group
	data-testid="filter-strip"
	class="flex flex-wrap items-center gap-2"
>
	{#each clauses as clause (keyOf(clause))}
		<Chip
			removeLabel={m.search_filters_remove({ label: clause.label.text })}
			fallbackFocus={() => el}
			onremove={() => void run(() => onremove(clause))}
		>
			{@render text(clause.label)}
		</Chip>
	{/each}
	{#if hidden}
		<Chip
			removeLabel={m.search_filters_remove({ label: hiddenLabel })}
			fallbackFocus={() => el}
			onremove={() => void run(() => onremovehidden(hiddenLabel))}
		>
			{hiddenLabel}
		</Chip>
	{/if}
	{#if count}
		<Button variant="quiet" dense onclick={() => void run(onclear)}>
			{m.search_filters_clear_all()}
		</Button>
	{/if}
</div>
{#if failed}
	<Banner kind="error" title={m.search_filters_change_failed()} class="mt-2">
		{m.search_filters_change_failed_body()}
		{#snippet action()}
			<Button dense onclick={() => failed && void run(failed)}>{m.search_query_retry()}</Button>
		{/snippet}
	</Banner>
{/if}
