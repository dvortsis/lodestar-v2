<script lang="ts">
	// Table mode (§10.4): a real <table> with a sticky header, sortable columns (they write W5's
	// sort: no client-side resort of a paged list), numbers right-aligned with units, and a column
	// chooser. The scroll container is the table's own, so narrow widths scroll here, not the page.
	// Rows are a roving tab stop like the list's; j/k and the arrows move, Enter opens. More pages
	// arrive from the sentinel row (the panel's "Load more" is the fallback).
	import { onMount, tick } from 'svelte';
	import ArrowDown from '~icons/ph/arrow-down-bold';
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import Columns from '~icons/ph/columns-bold';
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { DEFAULT_SORT, search } from '$lib/state/search.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import { showBidi } from '$lib/util/bidi';
	import { TYPE_LABELS as TYPE } from '$lib/util/content-labels';
	import { ensureSearchPath } from '../query/path';
	import type { SearchHit } from '../results/ResultRow.svelte';
	import { pick, selectAll } from '../bulk/ops';
	import { selection } from '../bulk/selection.svelte';
	import {
		NUMERIC,
		SORT_FIELD,
		ariaSort,
		nextSort,
		readPrefs,
		visible,
		writePrefs,
		type ColumnId,
		type ColumnPrefs
	} from './columns';
	import './table.css';

	interface Props {
		hits: SearchHit[];
		hasMore: boolean;
		onLoadMore: () => void;
		onOpen: (hit: SearchHit) => void;
		active: number;
	}
	let { hits, hasMore, onLoadMore, onOpen, active = $bindable(0) }: Props = $props();

	const locale = $derived(currentLocale());
	let prefs = $state<ColumnPrefs>(readPrefs());
	const shown = $derived(visible(prefs));
	const checks = $derived(selection.mode || selection.count > 0);
	const everyRow = $derived(hits.length > 0 && hits.every((h) => selection.has(h.hash)));

	const LABEL: Record<ColumnId, () => string> = {
		name: m.search_table_col_name,
		type: m.search_table_col_type,
		size: m.search_table_col_size,
		files: m.search_table_col_files,
		seeders: m.seeders,
		leechers: m.leechers,
		trackers: m.trackers_reporting,
		discovered: m.discovered
	};

	function sortBy(field: string) {
		ensureSearchPath();
		void search.setSort(nextSort(search.sort === DEFAULT_SORT ? DEFAULT_SORT : search.sort, field));
	}

	function save(next: ColumnPrefs) {
		prefs = next;
		writePrefs(next);
	}

	let Chooser = $state<typeof import('./ColumnChooser.svelte').default>();
	let choosing = $state(false);
	async function choose() {
		Chooser ??= (await import('./ColumnChooser.svelte')).default;
		choosing = !choosing;
	}

	function close() {
		choosing = false;
		void tick().then(() =>
			scroller?.parentElement?.querySelector<HTMLElement>('[data-testid="table-columns"]')?.focus()
		);
	}

	let scroller = $state<HTMLElement>();
	let sentinel = $state<HTMLElement>();
	onMount(() => {
		if (!sentinel || !scroller || typeof IntersectionObserver === 'undefined') return;
		const io = new IntersectionObserver(
			(entries) => {
				if (entries.some((e) => e.isIntersecting)) onLoadMore();
			},
			{ root: scroller, rootMargin: '200px' }
		);
		io.observe(sentinel);
		return () => io.disconnect();
	});
	// A page that doesn't fill the scroller leaves the sentinel in view and so never leaves it:
	// ask again whenever rows arrive while it is still visible.
	$effect(() => {
		void hits.length;
		if (!hasMore || !sentinel || !scroller) return;
		const s = scroller.getBoundingClientRect();
		const r = sentinel.getBoundingClientRect();
		if (r.top < s.bottom + 200) onLoadMore();
	});

	function rowEl(i: number) {
		return scroller?.querySelector<HTMLElement>(`tbody tr[data-index="${i}"]`) ?? null;
	}
	/** The same handle the list gives the panel (j/k, the skip link). */
	export function moveTo(index: number) {
		active = Math.min(hits.length - 1, Math.max(0, index));
		void tick().then(() => rowEl(active)?.focus());
	}
	export function move(delta: number) {
		moveTo(active + delta);
	}

	function keyDown(event: KeyboardEvent, i: number, hit: SearchHit) {
		if ((event.target as HTMLElement).closest('input, button, a')) return;
		if (event.key === 'ArrowDown') moveTo(i + 1);
		else if (event.key === 'ArrowUp') moveTo(i - 1);
		else if (event.key === 'Home') moveTo(0);
		else if (event.key === 'End') moveTo(hits.length - 1);
		else if (event.key === 'Enter') onOpen(hit);
		else return;
		event.preventDefault();
	}
</script>

<div class="rt" data-testid="results-table-wrap">
	<div class="rt-bar">
		<Button
			dense
			aria-expanded={choosing}
			onclick={() => void choose()}
			data-testid="table-columns"
		>
			<Columns aria-hidden="true" />
			{m.search_table_columns()}
		</Button>
	</div>
	{#if Chooser && choosing}
		<Chooser {prefs} onchange={save} onclose={close} />
	{/if}
	<div class="rt-scroll" bind:this={scroller} tabindex="-1">
		<table class="rt-table" data-testid="results-table" aria-label={m.search_results_label()}>
			<thead>
				<tr>
					{#if checks}
						<th scope="col" class="rt-check">
							<input
								type="checkbox"
								aria-label={m.search_bulk_select_all()}
								checked={everyRow}
								data-testid="table-select-all"
								onchange={() => selectAll(hits.map((h) => h.hash))}
							/>
						</th>
					{/if}
					{#each shown as id (id)}
						{@const field = SORT_FIELD[id]}
						<th
							scope="col"
							class:rt-num={NUMERIC.has(id)}
							aria-sort={field ? ariaSort(search.sort, field) : undefined}
							data-testid="table-th-{id}"
						>
							{#if field}
								<button type="button" class="rt-sort" onclick={() => sortBy(field)}>
									{LABEL[id]()}
									{#if ariaSort(search.sort, field) === 'ascending'}<ArrowUp aria-hidden="true" />
									{:else if ariaSort(search.sort, field) === 'descending'}<ArrowDown
											aria-hidden="true"
										/>{/if}
								</button>
							{:else}
								{LABEL[id]()}
							{/if}
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each hits as hit, i (hit.hash)}
					{@const selected = selection.has(hit.hash)}
					<tr
						data-index={i}
						data-testid="table-row-{i + 1}"
						tabindex={i === active ? 0 : -1}
						aria-selected={checks ? selected : undefined}
						class:rt-selected={selected}
						onfocus={() => (active = i)}
						onclick={() => onOpen(hit)}
						onkeydown={(e) => keyDown(e, i, hit)}
					>
						{#if checks}
							<td class="rt-check">
								<input
									type="checkbox"
									tabindex="-1"
									checked={selected}
									aria-label={m.search_bulk_select_row({ name: hit.title ?? hit.name })}
									data-testid="table-select-{i + 1}"
									onclick={(e) =>
										pick(
											e,
											hit.hash,
											hits.map((h) => h.hash)
										)}
								/>
							</td>
						{/if}
						{#each shown as id (id)}
							{#if id === 'name'}
								<th scope="row" class="rt-name" data-hash={hit.hash}>
									<span class="rt-title" data-testid="table-title-{i + 1}"
										>{showBidi(hit.title ?? hit.name)}</span
									>
								</th>
							{:else}
								<td class:rt-num={NUMERIC.has(id)} data-testid="table-{id}-{i + 1}">
									{#if id === 'type'}
										{(hit.content_type && TYPE[hit.content_type]?.()) || '—'}
									{:else if id === 'size'}
										{formatBytes(hit.size, locale)}
									{:else if id === 'files'}
										{formatCount(hit.files, locale)}
									{:else if id === 'seeders'}
										{#if hit.seeders === null}<span role="img" aria-label={m.never_scraped()}
												>—</span
											>{:else}{formatCount(
												{ value: hit.seeders, approx: hit.seeders_approx },
												locale
											)}{/if}
									{:else if id === 'leechers'}
										{#if hit.leechers === null}<span role="img" aria-label={m.never_scraped()}
												>—</span
											>{:else}{formatCount(
												{ value: hit.leechers, approx: hit.seeders_approx },
												locale
											)}{/if}
									{:else if id === 'trackers'}
										{#if hit.trackers_reporting === null || hit.trackers_checked === null}<span
												role="img"
												aria-label={m.never_scraped()}>—</span
											>{:else}{m.search_results_trackers_of({
												reporting: formatCount(hit.trackers_reporting, locale),
												checked: formatCount(hit.trackers_checked, locale)
											})}{/if}
									{:else if id === 'discovered'}
										<RelativeTime seconds={hit.discovered_at} />
									{/if}
								</td>
							{/if}
						{/each}
					</tr>
				{/each}
			</tbody>
		</table>
		<div
			bind:this={sentinel}
			class="rt-sentinel"
			data-testid="table-sentinel"
			aria-hidden="true"
		></div>
	</div>
</div>
