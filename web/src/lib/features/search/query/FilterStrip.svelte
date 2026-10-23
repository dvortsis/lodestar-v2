<script lang="ts">
	// The active-filter strip (§10.4): one removable chip per structured clause, the `hidden=1` chip
	// and "Clear all". Loaded only once there is a chip. Changes swap in place, with no animation
	// (§10.2: filter changes never re-animate).
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
		}
	}

	const count = $derived(clauses.length + (hidden ? 1 : 0));

	// Keyed by label, not position: removing one chip must not re-create its neighbours, which
	// would take the focus Chip hands to the next one.
	const keyed = $derived(
		clauses.map((clause, i) => ({
			clause,
			key: `${clause.label.text}#${clauses.slice(0, i).filter((c) => c.label.text === clause.label.text).length}`
		}))
	);

	async function clear() {
		await run(onclear);
		// The strip goes with its last chip: focus the editor rather than <body>.
		if (!failed && (!document.activeElement || document.activeElement === document.body))
			el.focus();
	}
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
	{#each keyed as { clause, key } (key)}
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
		<Button variant="quiet" dense onclick={() => void clear()}>
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
