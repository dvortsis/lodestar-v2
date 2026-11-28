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
	import GroupToggle from '../collapse/GroupToggle.svelte';
	import { groupsCount } from '../collapse/count';
	import { PARAM, groupHref, groupedIn, grouping } from '../collapse/grouping.svelte';
	import { viewOf, type View } from './view';
	import { FOCUS_RING } from '$lib/ui/classes';
	import { api } from '$lib/api/client';
	import type { components } from '$lib/api/schema';

	const id = $props.id();
	const locale = $derived(currentLocale());

	const sorts = $derived(SORTS.map((s) => ({ value: s.value, label: s.label() })));
	const sort = $derived(sortValue(search.sort));
	// The account's Appearance density is the default view. Like the rows, the toolbar reads it with
	// the URL, so a preference the shell applies late waits for the next query (view.ts).
	const pref = () => document.documentElement.dataset.density;
	// From the query string, so a new URL object with the same query (the drawer closing) re-reads nothing.
	const qs = $derived(page.url.search);
	const view = $derived(viewOf(qs, pref()));
	const views = $derived(VIEWS.map((v) => ({ value: v, label: viewLabels[v]() })));
	const viewLabels: Record<View, () => string> = {
		list: m.search_table_view_list,
		compact: m.search_toolbar_compact,
		table: m.search_table_view_table,
		cards: m.search_cards_option
	};

	// Below x-large the Filters host is out of flow (toolbar.css): this component, alive in every
	// state, tells #results how much room it takes.
	$effect(() => {
		const rail = document.getElementById('search-rail');
		const region = document.getElementById('results');
		if (!rail || !region || typeof ResizeObserver !== 'function') return;
		const set = () => {
			const w = rail.offsetWidth;
			const gap = parseFloat(getComputedStyle(rail.parentElement!).rowGap) || 0;
			region.style.setProperty('--tb-inset', w ? `${w + 8}px` : '0px');
			region.style.setProperty('--tb-rail-h', `${rail.offsetHeight + gap}px`);
		};
		set(); // before the first paint: the observer's first call comes a frame later
		const ro = new ResizeObserver(set);
		ro.observe(rail);
		return () => ro.disconnect();
	});

	const toggle = () => (selection.mode = !selection.mode);
	const shut = (e: MouseEvent) =>
		(e.currentTarget as HTMLElement).closest<HTMLElement>('[popover]')?.hidePopover();

	// The count names groups and torrents (a long line): the hidden note only fits it from 71rem of
	// toolbar (toolbar.css), so it is told apart by a class.
	const isGroups = $derived(
		results.status === 'ready' &&
			results.hits.length > 0 &&
			results.grouped.collapsed &&
			results.grouped.groups !== undefined
	);
	// The count of the rows shown: none while the first answer is out, or after it failed.
	const count = $derived.by(() => {
		if (results.status !== 'ready') return '';
		const n = results.hits.length === 0 ? 0 : results.total;
		const g = results.grouped;
		if (g.collapsed && g.groups !== undefined && n > 0)
			return groupsCount(g.groups, !!g.groups_approx, n, results.totalApprox, locale);
		if (results.totalApprox)
			return m.search_toolbar_count_about({ count: formatCompact(n, locale) });
		return n === 1
			? m.search_toolbar_count_one()
			: m.search_toolbar_count_other({ count: formatCount(n, locale) });
	});

	// "N hidden · Show" (§10.4, P7): matches a filter rule hid, as the server counts them for the
	// rows shown (POST /search/hidden). Asked once the rows have painted, so the first page never
	// waits for it; a newer query, sort or visibility aborts it, and a late answer is dropped. "Show"
	// adds exactly those (`hidden=true`); none to offer, or a failure, shows nothing. Its 44 px target
	// under a coarse pointer is padding taken back by a negative margin, so the line keeps its height.
	type Body = components['schemas']['SearchRequest'];
	let hidden = $state(0);
	$effect(() => {
		const q = results.query;
		hidden = 0;
		if (!q || q.hidden) return;
		const ctl = new AbortController();
		const frame = requestAnimationFrame(() =>
			api
				.post('/search/hidden', {
					// The rest of a search's body is left unset (the server ignores it here).
					body: { ast: q.ast, hidden: false } as Body,
					signal: ctl.signal
				})
				.then((r) => {
					if (!ctl.signal.aborted) hidden = r.hidden_total ?? 0;
				})
				.catch(() => {})
		);
		return () => {
			cancelAnimationFrame(frame);
			ctl.abort();
		};
	});
	function showHidden() {
		ensureSearchPath();
		void search.setHidden(true);
		// The button goes with its count: focus the results it opened (as Skip to results does), not
		// the page.
		const region = document.getElementById('results');
		region?.setAttribute('tabindex', '-1');
		region?.focus();
	}

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

	// "Group versions" (GS-5): pushed like a sort, so Back undoes it; remembered for the account.
	// As the panel reads it: the URL, else the account's choice the rows were asked with.
	const grouped = $derived(groupedIn(qs, grouping.applied));
	const unavailable = $derived(grouped && !!results.grouped.collapse_unavailable);
	async function setGrouped(on: boolean) {
		ensureSearchPath();
		const was = grouped;
		void grouping.remember(on);
		const opts = { keepFocus: true, noScroll: true };
		// The entry being left writes its own value out first, so Back restores it whatever the
		// account's choice is by then (the same rows: no new request).
		if (!new URLSearchParams(page.url.search).has(PARAM))
			// eslint-disable-next-line svelte/no-navigation-without-resolve -- groupHref() keeps this page's own path
			await goto(groupHref(page.url, was), { ...opts, replaceState: true });
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- groupHref() keeps this page's own path
		await goto(groupHref(page.url, on), opts);
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
	<div class="tb" data-testid="results-toolbar">
		<p class="tb-count" class:tb-grp={isGroups} data-testid="results-count">
			{count}{#if hidden > 0 && results.status === 'ready' && !results.refining}<span class="tb-hid"
					>&nbsp;· <span id="{id}-hidden" class="tb-full" data-testid="results-hidden"
						>{m.search_toolbar_hidden({ count: formatCount(hidden, locale) })}</span
					><span class="tb-short" aria-hidden="true"
						>{m.search_toolbar_hidden_short({ count: formatCount(hidden, locale) })}</span
					>&nbsp;·
					<button
						type="button"
						class="text-primary underline pointer-coarse:-m-3 pointer-coarse:min-h-0 pointer-coarse:p-3 {FOCUS_RING}"
						data-testid="results-hidden-show"
						aria-describedby="{id}-hidden"
						onclick={showHidden}>{m.search_toolbar_hidden_show()}</button
					></span
				>{/if}
		</p>
		<div class="tb-controls">
			<GroupToggle
				on={grouped}
				describedby={unavailable ? `${id}-ungrouped` : undefined}
				class="tb-group"
				onchange={(on) => void setGrouped(on)}
			/>
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
			     Tab moves through it, Escape closes it and returns focus). Group versions joins them there
			     whenever the toolbar is too narrow for it. CSS picks (toolbar.css). -->
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
					<GroupToggle
						on={grouped}
						describedby={unavailable ? `${id}-ungrouped` : undefined}
						onchange={(on) => void setGrouped(on)}
					/>
					<Button
						dense
						aria-pressed={selection.mode}
						class="tb-phone"
						onclick={(e) => (toggle(), shut(e))}>{m.search_bulk_mode()}</Button
					>
					<Button dense class="tb-phone" onclick={(e) => (void save(), shut(e))}
						>{m.search_toolbar_save()}</Button
					>
				</div>
			</div>
		</div>
		{#if unavailable}
			<!-- Owner §2.66: one line on why the rows came ungrouped, under the controls. -->
			<p
				id="{id}-ungrouped"
				class="basis-full text-small text-muted-text"
				data-testid="group-versions-unavailable"
			>
				{m.search_collapse_unavailable()}
			</p>
		{/if}
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
