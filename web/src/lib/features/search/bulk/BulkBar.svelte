<script lang="ts">
	// The bulk bar (§10.4): Copy magnets, Export and Compare (2–5) for the selected rows. Each reports
	// through a toast; the count is announced by the panel's polite region. Send waits for an API to
	// send to (docs/decisions/2026-10-31-S-search-5.md). Loaded when the first row is selected.
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { compareHref } from '$lib/features/compare/href';
	import {
		COMPARE_MAX,
		COMPARE_MIN,
		EXPORT_MAX,
		canCompare,
		copyMagnets,
		download,
		exportHashes
	} from './actions';
	import './bulk.css';
	import { selection } from './selection.svelte';

	const locale = $derived(currentLocale());
	const n = $derived(selection.count);
	let busy = $state<'copy' | 'export' | null>(null);

	async function copy() {
		busy = 'copy';
		const r = await copyMagnets(selection.hashes);
		busy = null;
		if (!r.ok)
			toast.push({
				kind: 'error',
				text:
					r.reason === 'too_many'
						? m.search_bulk_too_many({ max: formatCount(EXPORT_MAX, locale) })
						: m.search_bulk_failed()
			});
		else if (!r.copied) {
			// The clipboard refused (a slow answer ends the click's permission): the detail view's
			// copy shows the text in its manual field.
			void import('$lib/features/detail/magnet').then((mod) => mod.copyMagnet({ magnet: r.text }));
		} else
			toast.push({
				kind: 'success',
				text: (r.count === 1 ? m.search_bulk_copied_one : m.search_bulk_copied_other)({
					count: formatCount(r.count, locale)
				})
			});
	}

	async function exportFile() {
		busy = 'export';
		const r = await exportHashes(selection.hashes, 'csv');
		busy = null;
		if (!r.ok)
			return void toast.push({
				kind: 'error',
				text:
					r.reason === 'too_many'
						? m.search_bulk_too_many({ max: formatCount(EXPORT_MAX, locale) })
						: m.search_bulk_failed()
			});
		download(r.text, r.filename, 'text/csv');
		toast.push({
			kind: 'success',
			text: m.search_bulk_exported({ count: formatCount(n, locale) })
		});
	}

	function compare() {
		const density = page.url.searchParams.get('density');
		const href = compareHref(selection.hashes, new URLSearchParams(density ? { density } : {}));
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- compareHref() resolves
		void goto(href);
	}
</script>

<div class="bb" role="region" aria-label={m.search_bulk_label()} data-testid="bulk-bar">
	<p class="bb-count" data-testid="bulk-count">
		{(n === 1 ? m.search_bulk_count_one : m.search_bulk_count_other)({
			count: formatCount(n, locale)
		})}
	</p>
	<Button dense loading={busy === 'copy'} onclick={() => void copy()} data-testid="bulk-copy"
		>{m.search_bulk_copy()}</Button
	>
	<Button
		dense
		loading={busy === 'export'}
		onclick={() => void exportFile()}
		data-testid="bulk-export">{m.search_bulk_export()}</Button
	>
	<Button dense disabled={!canCompare(n)} onclick={compare} data-testid="bulk-compare"
		>{m.search_bulk_compare()}</Button
	>
	{#if !canCompare(n)}
		<p class="bb-hint" data-testid="bulk-compare-hint">
			{m.search_bulk_compare_hint({ min: COMPARE_MIN, max: COMPARE_MAX })}
		</p>
	{/if}
	<Button dense variant="quiet" onclick={() => (selection.mode = false)} data-testid="bulk-clear"
		>{m.search_bulk_clear()}</Button
	>
</div>
