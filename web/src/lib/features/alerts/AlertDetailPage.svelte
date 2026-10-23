<script lang="ts">
	// One alert (§10.8): the saved search's header and its alerts, newest first, with a "New
	// since …" mark where the server's `read` flags change. New alerts never move the list under
	// the reader: an event only raises the "N new · Show" banner (§10.4's Live rule).
	import { goto } from '$app/navigation';
	import { onMount, tick } from 'svelte';
	import IconTrash from '~icons/ph/trash';
	import IconPause from '~icons/ph/pause';
	import IconSearch from '~icons/ph/magnifying-glass';
	import { events } from '$lib/state/events.svelte';
	import { keys } from '$lib/keys';
	import { formatCount, formatDateTime } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import VirtualList from '$lib/ui/lists/VirtualList.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import {
		alertsPath,
		feedPath,
		searchPath,
		settingsSecurityPath,
		torrentPath,
		type AlertRow,
		type SavedSearch
	} from './api';
	import { createAlertDetail } from './detail.svelte';
	import { throttled } from './throttle';
	import AlertHitRow from './AlertHitRow.svelte';
	import ChannelBadges from './ChannelBadges.svelte';
	import PauseToggle from './PauseToggle.svelte';

	let { id }: { id: number } = $props();

	// Moving from one alert to another starts clean: the state follows the id.
	const d = $derived(createAlertDetail(id));
	const locale = $derived(currentLocale());

	$effect(() => {
		void d.load();
		return () => d.cancel();
	});
	// Subscribe once there is data (see the list page: the shell's 401 hand-off comes first).
	const loaded = $derived(d.search !== undefined);
	$effect(() => {
		if (!loaded) return;
		return events.on(
			'alerts',
			(event) => {
				if (event.saved_search === id) d.refreshSoon();
			},
			{ onResync: () => d.refreshSoon() }
		);
	});

	// --- the list: alerts and, once, the mark ---------------------------------------------
	type Item =
		{ kind: 'alert'; key: string; row: AlertRow } | { kind: 'mark'; key: string; at: number };
	const items = $derived.by<Item[]>(() => {
		const out: Item[] = d.rows.map((row) => ({ kind: 'alert', key: `a${row.id}`, row }));
		if (d.mark) out.splice(d.mark.before, 0, { kind: 'mark', key: 'mark', at: d.mark.at });
		return out;
	});
	let active = $state(0);

	function open(_: number, item: Item) {
		if (item.kind !== 'alert') return;
		const path = torrentPath(item.row.torrent.hash);
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- torrentPath() resolves
		if (path) void goto(path);
	}

	// j / k step through the alerts (registered below), skipping the mark.
	let listEl = $state<HTMLElement>();
	async function step(by: 1 | -1) {
		let next = active + by;
		if (items[next]?.kind === 'mark') next += by;
		if (next < 0 || next >= items.length) return;
		active = next;
		await tick();
		await tick();
		listEl?.querySelector<HTMLElement>(`[data-index="${next}"]`)?.focus();
	}
	// onMount, not $effect: registering writes the registry's version, and an effect that reads and
	// writes the same state loops.
	onMount(() => {
		const pop = keys.pushScope('alerts');
		const offs = [
			keys.register({
				id: 'results.next',
				title: () => m.alerts_key_next(),
				section: () => m.alerts_section(),
				scope: 'alerts',
				palette: false,
				run: () => void step(1)
			}),
			keys.register({
				id: 'results.previous',
				title: () => m.alerts_key_previous(),
				section: () => m.alerts_section(),
				scope: 'alerts',
				palette: false,
				run: () => void step(-1)
			})
		];
		return () => {
			offs.forEach((off) => off());
			pop();
		};
	});

	// --- the banner and its polite, throttled announcement ----------------------------------
	const LIVE_THROTTLE_MS = 10_000;
	let announcement = $state('');
	const bannerText = (n: number) =>
		n === 1 ? m.alerts_banner_one() : m.alerts_banner_many({ count: formatCount(n, locale) });
	const announce = throttled<string>(LIVE_THROTTLE_MS, (text) => (announcement = text));
	$effect(() => {
		if (d.fresh > 0) announce.set(bannerText(d.fresh));
		else announce.reset('');
	});
	$effect(() => () => announce.cancel());

	// --- pause, delete ------------------------------------------------------------------------
	let DeleteDialog = $state<typeof import('./DeleteAlertDialog.svelte').default>();
	let deleteOpen = $state(false);
	async function openDelete() {
		try {
			DeleteDialog ??= (await import('./DeleteAlertDialog.svelte')).default;
		} catch {
			toast.push({ kind: 'error', text: m.alerts_error_load() });
			return;
		}
		deleteOpen = true;
	}
	function deleted(s: Pick<SavedSearch, 'name'>) {
		toast.push({ kind: 'success', text: m.alerts_deleted({ name: s.name }) });
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- alertsPath() resolves
		void goto(alertsPath());
	}

	const notFound = $derived(d.error?.code === 'saved_search_not_found');
	const feed = $derived(d.search?.rss ? feedPath(d.search.id) : undefined);
	const loadingFirst = $derived(d.loading && d.search === undefined);
	// A load that ends within 300 ms shows no skeleton (no flash); the space is held meanwhile.
	let slow = $state(false);
	$effect(() => {
		if (!loadingFirst) return;
		const t = setTimeout(() => (slow = true), 300);
		return () => clearTimeout(t);
	});
</script>

<section class="al-page" aria-busy={d.loading || undefined}>
	{#if d.error && !d.search}
		<div class="al-missing">
			<h1 class="al-h1">{notFound ? m.alerts_not_found_title() : m.alerts_title()}</h1>
			{#if notFound}
				<p class="al-muted">{m.alerts_not_found_text()}</p>
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- alertsPath() resolves -->
				<a href={alertsPath()} class="btn btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
					>{m.alerts_back()}</a
				>
			{:else}
				<Banner kind="error">
					{m.alerts_error_load()}
					{#snippet action()}
						<Button dense onclick={() => void d.load()}>{m.alerts_retry()}</Button>
					{/snippet}
				</Banner>
			{/if}
		</div>
	{:else}
		<header class="al-header">
			<div class="al-title-row">
				<h1 class="al-h1" data-testid="alert-name">
					{d.search ? d.search.name : m.alerts_title()}
				</h1>
				{#if d.search?.paused}
					<Badge variant="warning"
						><IconPause class="size-4 shrink-0" aria-hidden="true" /><span
							data-testid="alert-paused">{m.alerts_paused()}</span
						></Badge
					>
				{/if}
			</div>
			{#if d.search}
				{@const s = d.search}
				<div class="al-query">
					{#if s.q.trim() === ''}
						<span class="al-muted">{m.alerts_query_all()}</span>
					{:else}
						<Mono wrap>{s.q}</Mono>
					{/if}
					<!-- eslint-disable svelte/no-navigation-without-resolve -- searchPath() resolves -->
					<a
						href={searchPath(s)}
						class="btn btn-outline btn-sm border-field-border {FOCUS_RING} {TARGET_MIN}"
						data-testid="alert-open-search"
						><IconSearch class="size-4" aria-hidden="true" />{m.alerts_open_in_search()}</a
					>
					<!-- eslint-enable svelte/no-navigation-without-resolve -->
				</div>
				<div class="al-meta">
					<span data-testid="alert-opened-unread"
						>{m.alerts_new_count({ count: formatCount(d.opened, locale) })}</span
					>
					<ChannelBadges search={s} />
					<PauseToggle search={s} onUpdated={(u) => d.replace(u)} />
					<Button
						variant="quiet"
						dense
						aria-label={m.alerts_delete_aria({ name: s.name })}
						onclick={() => void openDelete()}
						data-testid="alert-delete"
					>
						<IconTrash class="size-4" aria-hidden="true" />{m.alerts_delete()}
					</Button>
				</div>
				{#if feed}
					<div class="al-feed">
						<label class="al-feed-label" for="alert-feed">{m.alerts_feed_label()}</label>
						<Input
							id="alert-feed"
							mono
							readonly
							value={feed}
							onfocus={(e) => e.currentTarget.select()}
							data-testid="alert-feed"
						/>
						<p class="al-muted">
							{m.alerts_feed_help()}
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- settingsSecurityPath() resolves -->
							<a href={settingsSecurityPath()} class="al-link">{m.alerts_feed_help_link()}</a>.
						</p>
					</div>
				{/if}
			{:else if loadingFirst}
				<Skeleton lines={2} />
			{/if}
		</header>

		{#if d.error && d.search}
			<Banner kind="error">
				{m.alerts_error_refresh()}
				{#snippet action()}
					<Button dense onclick={() => void d.retry()}>{m.alerts_retry()}</Button>
				{/snippet}
			</Banner>
		{/if}

		{#if d.fresh > 0}
			<Banner kind="info">
				<span data-testid="alert-banner">{bannerText(d.fresh)}</span>
				{#snippet action()}
					<Button dense onclick={() => void d.show()} data-testid="alert-show"
						>{m.alerts_show()}</Button
					>
				{/snippet}
			</Banner>
		{/if}
		<div class="sr-only" role="status" aria-live="polite" data-testid="alert-live">
			{announcement}
		</div>

		{#if loadingFirst}
			<div class="al-skeleton" data-testid="alert-skeleton">
				{#if slow}
					{#each [0, 1, 2] as n (n)}
						<Skeleton class="al-skeleton-row" />
					{/each}
				{/if}
			</div>
		{/if}
		{#if d.search}
			{#if items.length === 0 && !d.loading}
				<p class="al-empty" data-testid="alert-empty">
					{d.search.paused ? m.alerts_empty_detail_paused() : m.alerts_empty_detail()}
				</p>
			{:else}
				<div class="al-list" bind:this={listEl}>
					<VirtualList
						{items}
						role="grid"
						getKey={(it) => it.key}
						label={m.alerts_matches_label({ name: d.search.name })}
						estimateSize={112}
						measureRows
						bind:activeIndex={active}
						onActivate={open}
						hasMore={d.hasMore}
						loadingMore={d.loadingMore}
						onLoadMore={() => void d.loadMore()}
						loadMoreLabel={m.alerts_loading()}
					>
						{#snippet row(it)}
							{#if it.kind === 'mark'}
								<div role="gridcell" class="al-divider-cell">
									<h2 class="al-divider" data-testid="alert-divider">
										{m.alerts_divider({ time: formatDateTime(it.at, locale) })}
									</h2>
								</div>
							{:else}
								<div role="gridcell"><AlertHitRow row={it.row} /></div>
							{/if}
						{/snippet}
					</VirtualList>
				</div>
				{#if d.moreError}
					<Banner kind="error">
						{m.alerts_error_more()}
						{#snippet action()}
							<Button dense onclick={() => void d.retryMore()}>{m.alerts_retry()}</Button>
						{/snippet}
					</Banner>
				{/if}
			{/if}
		{/if}
	{/if}
</section>

{#if DeleteDialog && d.search}
	<DeleteDialog bind:open={deleteOpen} search={d.search} onDeleted={deleted} />
{/if}

<style>
	.al-page {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
		inline-size: 100%;
		max-inline-size: calc(var(--rail-width) * 5);
		margin-inline: auto;
		padding: calc(var(--spacing) * 4);
	}
	@media (min-width: 40rem) {
		.al-page {
			padding: calc(var(--spacing) * 6);
		}
	}
	.al-header {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 3);
	}
	.al-title-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: calc(var(--spacing) * 3);
	}
	.al-h1 {
		margin: 0;
		overflow-wrap: anywhere;
		font-size: var(--text-display);
		line-height: var(--text-display--line-height);
		letter-spacing: var(--text-display--letter-spacing);
		font-weight: var(--font-weight-semibold);
	}
	.al-query,
	.al-meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: calc(var(--spacing) * 3);
	}
	.al-meta {
		align-items: flex-start;
	}
	.al-feed {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
		max-inline-size: var(--manuscript-width);
	}
	.al-feed-label {
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-weight: var(--font-weight-medium);
	}
	.al-muted {
		margin: 0;
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
	}
	.al-link {
		text-decoration: underline;
		text-underline-offset: calc(var(--spacing) * 1);
	}
	.al-missing {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: calc(var(--spacing) * 3);
	}
	.al-skeleton {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
		min-block-size: calc(var(--row-height-comfortable) * 3 + var(--spacing) * 4);
	}
	.al-page :global(.al-skeleton-row) {
		block-size: var(--row-height-comfortable);
	}
	.al-empty {
		margin: 0;
		padding: calc(var(--spacing) * 6) 0;
		color: var(--color-muted-text);
	}
	.al-list {
		block-size: max(calc(var(--row-height-comfortable) * 6), calc(100dvh - var(--spacing) * 64));
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-box);
		background: var(--color-base-100);
		overflow: hidden;
	}
	.al-divider {
		display: flex;
		align-items: center;
		margin: 0;
		min-block-size: calc(var(--spacing) * 10);
		padding-inline: calc(var(--spacing) * 4);
		color: var(--color-base-content);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-weight: var(--font-weight-medium);
		background: var(--color-selected);
		border-block: var(--border) solid var(--color-primary);
	}
</style>
