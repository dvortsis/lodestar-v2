<script lang="ts">
	// The results toolbar (§10.4): parameters, kept apart from the filters, each one in the URL.
	// The count is visual only (2a's panel announces it); sort goes through W5, density is the
	// `density` parameter the rows read; Save search opens the alerts' dialog, loaded on first use.
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import Select from '$lib/ui/fields/Select.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { formatCompact, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { DEFAULT_SORT, search } from '$lib/state/search.svelte';
	import { ensureSearchPath } from '../query/path';
	import { results } from '../results/results.svelte';
	import { selection } from '../bulk/selection.svelte';
	import { SORTS, VIEWS, sortValue, viewHref } from './toolbar';
	import { viewOf, type View } from './view';

	const id = $props.id();
	const locale = $derived(currentLocale());

	const sorts = $derived(SORTS.map((s) => ({ value: s.value, label: s.label() })));
	const sort = $derived(sortValue(search.sort));
	// The account's Appearance density is the default view. Like the rows, the toolbar reads it with
	// the URL, so a preference the shell applies late waits for the next query (view.ts).
	const pref = () => document.documentElement.dataset.density;
	const view = $derived(viewOf(page.url, pref()));
	const views = $derived(VIEWS.map((v) => ({ value: v, label: viewLabels[v]() })));
	const viewLabels: Record<View, () => string> = {
		list: m.search_table_view_list,
		compact: m.search_toolbar_compact,
		table: m.search_table_view_table,
		cards: m.search_cards_option
	};

	// Below x-large the Filters host sits over this row's start (toolbar.css): the row leaves it the
	// host's measured width, plus a gap.
	let bar = $state<HTMLElement>();
	$effect(() => {
		const rail = document.getElementById('search-rail');
		if (!bar || !rail || typeof ResizeObserver !== 'function') return;
		const el = bar;
		const ro = new ResizeObserver(() => {
			const w = rail.offsetWidth;
			el.style.setProperty('--tb-inset', w ? `${w + 8}px` : '0px');
		});
		ro.observe(rail);
		return () => ro.disconnect();
	});

	const toggle = () => (selection.mode = !selection.mode);
	const shut = (e: MouseEvent) =>
		(e.currentTarget as HTMLElement).closest<HTMLElement>('[popover]')?.hidePopover();

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

	function setView(next: View) {
		ensureSearchPath();
		// A view parameter, not a search: it replaces the entry, so Back still walks searches.
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- viewHref() keeps this page's own path
		void goto(viewHref(page.url, next, pref()), {
			replaceState: true,
			keepFocus: true,
			noScroll: true
		});
	}

	let Dialog = $state<typeof import('$lib/features/alerts/SaveSearchDialog.svelte').default>();
	let saving = $state(false);
	let open = $state(false);
	async function save() {
		saving = true;
		try {
			// Two hops on purpose: save-dialog.ts holds the dialog's preload list (see that file).
			Dialog ??= (await (await import('./save-dialog')).load()).default;
			open = true;
		} catch {
			// The dialog's chunk didn't load (offline, or a new version replaced it): say so.
			toast.push({ kind: 'error', text: m.search_toolbar_save_failed() });
		} finally {
			saving = false;
		}
	}
</script>

{#if results.status !== 'empty'}
	<div class="tb" data-testid="results-toolbar" bind:this={bar}>
		<p class="tb-count" data-testid="results-count">{count}</p>
		<div class="tb-controls">
			<label class="tb-lbl" for="{id}-sort">
				<span>{m.search_toolbar_sort()}</span>
				<Select
					id="{id}-sort"
					dense
					options={sorts}
					value={sort}
					onchange={(e) => setSort(e.currentTarget.value)}
				/>
			</label>
			<label class="tb-lbl" for="{id}-view">
				<span>{m.search_table_view()}</span>
				<Select
					id="{id}-view"
					data-testid="view-select"
					dense
					options={views}
					value={view}
					onchange={(e) => setView(e.currentTarget.value as View)}
				/>
			</label>
			<!-- From 640 px Select and Save show; below it they sit in a disclosure (a native popover:
			     Tab moves through it, Escape closes it and returns focus). CSS picks one (toolbar.css). -->
			<Button
				dense
				aria-pressed={selection.mode}
				data-testid="select-mode"
				class="tb-wide"
				onclick={toggle}>{m.search_bulk_mode()}</Button
			>
			<Button dense loading={saving} class="tb-wide" onclick={() => void save()}
				>{m.search_toolbar_save()}</Button
			>
			<div class="tb-narrow">
				<Button dense popovertarget="{id}-more" class="tb-more">{m.search_toolbar_more()}</Button>
				<div popover="auto" id="{id}-more" class="tb-pop">
					<Button dense aria-pressed={selection.mode} onclick={(e) => (toggle(), shut(e))}
						>{m.search_bulk_mode()}</Button
					>
					<Button dense onclick={(e) => (void save(), shut(e))}>{m.search_toolbar_save()}</Button>
				</div>
			</div>
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
