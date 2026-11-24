<script lang="ts">
	// One row under an open group (UI review 02 §5.1), at the list's own height: another version as a
	// compact line, or the group's state (loading, failed, none) or its "Show all". The list opens
	// the row (Enter, a click): a version's drawer, the best version's for "Show all", a retry for a
	// failed load.
	import ArrowUp from '~icons/ph/arrow-up-bold';
	import Elbow from '~icons/ph/arrow-elbow-down-right-bold';
	import { formatBytes, formatCount, showBidi } from '../results/ResultRow.svelte';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import type { GroupRow } from './expand.svelte';

	interface Props {
		row: GroupRow;
		testid: string;
	}
	let { row, testid }: Props = $props();
	const locale = $derived(currentLocale());
	const parentTitle = $derived(showBidi(row.parent.title ?? row.parent.name));
</script>

<div class="cl-row" data-testid={testid} data-row={row.row}>
	<Elbow class="cl-elbow" aria-hidden="true" />
	{#if row.row === 'version'}
		{@const v = row.hit}
		<span class="sr-only">{m.search_collapse_version_of({ title: parentTitle })}: </span>
		<span class="cl-title" data-testid="{testid}-title">{showBidi(v.title ?? v.name)}</span>
		{#if v.resolution}<span class="cl-fact">{v.resolution}</span>{/if}
		<span class="cl-fact" data-testid="{testid}-size">{formatBytes(v.size, locale)}</span>
		<span class="cl-fact">
			<span class="sr-only">{m.seeders()}: </span><ArrowUp aria-hidden="true" />
			{#if v.seeders === null}<span role="img" aria-label={m.never_scraped()}>—</span>{:else}<span
					data-testid="{testid}-seeders"
					>{formatCount({ value: v.seeders, approx: v.seeders_approx }, locale)}</span
				>{/if}
		</span>
		<span class="cl-fact">
			<span class="sr-only">{m.discovered()}: </span><RelativeTime seconds={v.discovered_at} />
		</span>
	{:else if row.row === 'more'}
		<span class="cl-more" data-testid="{testid}-more"
			>{m.search_collapse_show_all({
				count: formatCount((row.parent.others ?? 0) + 1, locale)
			})}</span
		>
	{:else if row.row === 'loading'}
		<span class="cl-note" role="status">{m.search_collapse_loading()}</span>
	{:else if row.row === 'failed'}
		<span class="cl-note">{m.search_collapse_failed()}</span>
		<span class="cl-more">{m.search_collapse_retry()}</span>
	{:else}
		<span class="cl-note">{m.search_collapse_none()}</span>
	{/if}
</div>
