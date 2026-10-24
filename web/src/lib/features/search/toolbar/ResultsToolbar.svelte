<script lang="ts">
	// The results toolbar (§10.4): parameters, kept apart from the filters, each one in the URL.
	// The count is visual only (2a's panel announces it); sort goes through W5, density is the
	// `density` parameter the rows read; Save search opens the alerts' dialog, loaded on first use.
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import Select from '$lib/ui/fields/Select.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { formatCompact, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { DEFAULT_SORT, search } from '$lib/state/search.svelte';
	import { ensureSearchPath } from '../query/path';
	import { results } from '../results/results.svelte';
	import { SORTS, densityHref, sortValue } from './toolbar';

	const id = $props.id();
	const locale = $derived(currentLocale());

	const sorts = $derived(SORTS.map((s) => ({ value: s.value, label: s.label() })));
	const sort = $derived(sortValue(search.sort));
	const density = $derived(
		page.url.searchParams.get('density') === 'compact' ? 'compact' : 'comfortable'
	);

	// The count of the rows shown: none while the first answer is out, or after it failed.
	const count = $derived.by(() => {
		if (results.status !== 'ready') return '';
		const n = results.hits.length === 0 ? 0 : results.total;
		if (results.totalApprox)
			return m.search_toolbar_count_about({ count: formatCompact(n, locale) });
		return n === 1
			? m.search_toolbar_count_one()
			: m.search_toolbar_count_other({ count: formatCount(n, locale) });
	});

	// With the drawer open the URL is a shallow `/t/{hash}?…`: each write puts it back on /search
	// first (query/path.ts), which closes the drawer, rather than navigating to the detail page.
	function setSort(next: string) {
		ensureSearchPath();
		void search.setSort(next);
	}

	function setDensity(next: string) {
		ensureSearchPath();
		// A view parameter, not a search: it replaces the entry, so Back still walks searches.
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- densityHref() keeps this page's own path
		void goto(densityHref(page.url, next), { replaceState: true, keepFocus: true, noScroll: true });
	}

	let Dialog = $state<typeof import('$lib/features/alerts/SaveSearchDialog.svelte').default>();
	let saving = $state(false);
	let open = $state(false);
	async function save() {
		saving = true;
		try {
			Dialog ??= (await import('$lib/features/alerts/SaveSearchDialog.svelte')).default;
			open = true;
		} finally {
			saving = false;
		}
	}
</script>

{#if results.status !== 'empty'}
	<div class="tb" data-testid="results-toolbar">
		<p class="tb-count" data-testid="results-count">{count}</p>
		<div class="tb-controls">
			<label class="tb-sort" for="{id}-sort">
				<span>{m.search_toolbar_sort()}</span>
				<Select
					id="{id}-sort"
					dense
					options={sorts}
					value={sort}
					onchange={(e) => setSort(e.currentTarget.value)}
				/>
			</label>
			<SegmentedControl
				dense
				label={m.search_toolbar_density()}
				value={density}
				options={[
					{ value: 'comfortable', label: m.search_toolbar_comfortable() },
					{ value: 'compact', label: m.search_toolbar_compact() }
				]}
				onchange={setDensity}
			/>
			<Button dense loading={saving} onclick={() => void save()}>{m.search_toolbar_save()}</Button>
		</div>
	</div>
{/if}
{#if Dialog}
	<Dialog
		bind:open
		q={search.q}
		sort={search.sort === DEFAULT_SORT ? null : search.sort}
		onSaved={() => {}}
	/>
{/if}
