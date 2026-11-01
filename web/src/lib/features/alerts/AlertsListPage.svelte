<script lang="ts">
	// The Alerts list (§10.8): one row per saved search, in the server's order, with the server's
	// counts. A table from `large` up, a stacked list below, from one data path. Live: an `alerts`
	// event refetches the list, and the rows change in place (no reordering, no animation).
	import { onMount, tick } from 'svelte';
	import IconTrash from '~icons/ph/trash';
	import IconPause from '~icons/ph/pause';
	import StateScene from '$lib/shell/StateScene.svelte';
	import { events } from '$lib/state/events.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { alertPath, type SavedSearch } from './api';
	import { createAlertsList } from './list.svelte';
	import ChannelBadges from './ChannelBadges.svelte';
	import PauseToggle from './PauseToggle.svelte';

	const list = createAlertsList();
	const locale = $derived(currentLocale());

	// A table from `large` (40rem, the 640 px breakpoint) up; only one of the two is in the DOM.
	const query = '(min-width: 40rem)';
	let wide = $state(typeof matchMedia === 'function' && matchMedia(query).matches);
	$effect(() => {
		const mq = matchMedia(query);
		const onChange = () => (wide = mq.matches);
		onChange();
		mq.addEventListener('change', onChange);
		return () => mq.removeEventListener('change', onChange);
	});

	$effect(() => {
		void list.load();
		return () => list.cancel();
	});
	// Subscribe once there is data: a page that opens the stream before the shell has installed its
	// 401 hand-off would leave the stream stopped on a sign-in redirect nobody performs.
	const loaded = $derived(list.items !== undefined);
	$effect(() => {
		if (!loaded) return;
		return events.on('alerts', () => list.refreshSoon(), { onResync: () => void list.load() });
	});

	// --- dialogs, loaded on first use -------------------------------------------------------
	let SaveDialog = $state<typeof import('./SaveSearchDialog.svelte').default>();
	let DeleteDialog = $state<typeof import('./DeleteAlertDialog.svelte').default>();
	let saveOpen = $state(false);
	let deleteOpen = $state(false);
	let toDelete = $state<SavedSearch>();

	async function openCreate() {
		try {
			SaveDialog ??= (await import('./SaveSearchDialog.svelte')).default;
		} catch {
			toast.push({ kind: 'error', text: m.alerts_error_load() });
			return;
		}
		saveOpen = true;
	}
	async function openDelete(search: SavedSearch) {
		try {
			DeleteDialog ??= (await import('./DeleteAlertDialog.svelte')).default;
		} catch {
			toast.push({ kind: 'error', text: m.alerts_error_load() });
			return;
		}
		toDelete = search;
		deleteOpen = true;
	}

	let heading = $state<HTMLElement>();
	async function deleted(search: Pick<SavedSearch, 'id' | 'name'>) {
		// Focus goes to the next row's name, else the previous one's, else the heading.
		const rows = list.items ?? [];
		const at = rows.findIndex((s) => s.id === search.id);
		const neighbour = rows[at + 1] ?? rows[at - 1];
		list.remove(search.id);
		toast.push({ kind: 'success', text: m.alerts_deleted({ name: search.name }) });
		await tick();
		const target = neighbour
			? document.querySelector<HTMLElement>(`[data-alert-name="${neighbour.id}"]`)
			: null;
		(target ?? heading)?.focus();
		void list.load();
	}

	// onMount, not $effect: registering writes the registry's version, and an effect that reads and
	// writes the same state loops. Loaded after mount (commands.ts): the key registry stays out of
	// the page's first view, as on the alert page.
	onMount(() => {
		let off: (() => void) | undefined;
		let gone = false;
		void import('./commands').then(
			({ registerAlertsListCommands }) => {
				if (!gone) off = registerAlertsListCommands(() => void openCreate());
			},
			() => {}
		); // Shortcuts only: the page works without them.
		return () => {
			gone = true;
			off?.();
		};
	});

	const SKELETON_ROWS = 3;
	const SKELETON_DELAY_MS = 300;
	const showSkeleton = $derived(list.items === undefined && list.error === undefined);
	// A load that ends within 300 ms shows no skeleton (no flash); the space is held meanwhile.
	let slow = $state(false);
	$effect(() => {
		if (!showSkeleton) return;
		const t = setTimeout(() => (slow = true), SKELETON_DELAY_MS);
		return () => clearTimeout(t);
	});
</script>

<section class="al-page" aria-labelledby="alerts-heading" aria-busy={showSkeleton || undefined}>
	<div class="al-head">
		<h1 id="alerts-heading" class="al-h1" tabindex="-1" bind:this={heading}>{m.alerts_title()}</h1>
		<Button variant="primary" onclick={openCreate} data-testid="alerts-new">
			{m.alerts_new()}
		</Button>
	</div>

	{#if list.error}
		<Banner kind="error">
			{list.items ? m.alerts_error_refresh() : m.alerts_error_load()}
			{#snippet action()}
				<Button dense onclick={() => void list.load()}>{m.alerts_retry()}</Button>
			{/snippet}
		</Banner>
	{/if}

	{#if showSkeleton}
		<div class="al-skeleton" role="status" aria-label={m.alerts_loading()}>
			{#if slow}
				{#each Array.from({ length: SKELETON_ROWS }, (_, n) => n) as n (n)}
					<Skeleton class="al-skeleton-row" />
				{/each}
			{/if}
		</div>
	{:else if list.items && list.items.length === 0}
		<EmptyState title={m.alerts_empty_title()} text={m.alerts_empty_text()}>
			{#snippet illustration()}<StateScene id="empty_state_explore" />{/snippet}
		</EmptyState>
	{:else if list.items}
		{#snippet name(s: SavedSearch)}
			<!-- eslint-disable svelte/no-navigation-without-resolve -- alertPath() resolves -->
			<a
				href={alertPath(s.id)}
				class="al-name"
				data-alert-name={s.id}
				data-testid="alerts-name-{s.id}">{s.name}</a
			>
			<!-- eslint-enable svelte/no-navigation-without-resolve -->
		{/snippet}
		{#snippet queryText(s: SavedSearch)}
			{#if s.q.trim() === ''}
				<span class="al-muted">{m.alerts_query_all()}</span>
			{:else}
				<Mono wrap>{s.q}</Mono>
			{/if}
		{/snippet}
		{#snippet unread(s: SavedSearch)}
			<span class="al-num" data-testid="alerts-unread-{s.id}">{formatCount(s.unread, locale)}</span>
		{/snippet}
		{#snippet last(s: SavedSearch)}
			{#if s.last_alert_at === null}
				<span data-testid="alerts-last-{s.id}"
					><span aria-hidden="true">—</span><span class="sr-only">{m.alerts_no_alerts_yet()}</span
					></span
				>
			{:else}
				<span data-testid="alerts-last-{s.id}"><RelativeTime seconds={s.last_alert_at} /></span>
			{/if}
		{/snippet}
		{#snippet status(s: SavedSearch)}
			{#if s.paused}
				<Badge
					><IconPause class="size-4 shrink-0" aria-hidden="true" /><span
						data-testid="alerts-paused-{s.id}">{m.alerts_paused()}</span
					></Badge
				>
			{/if}
		{/snippet}
		{#snippet actions(s: SavedSearch)}
			<span class="al-actions">
				<PauseToggle search={s} onUpdated={(u) => list.replace(u)} />
				<Button
					variant="quiet"
					dense
					aria-label={m.alerts_delete_aria({ name: s.name })}
					onclick={() => void openDelete(s)}
					data-testid="alerts-delete-{s.id}"
				>
					<IconTrash class="size-4" aria-hidden="true" />{m.alerts_delete()}
				</Button>
			</span>
		{/snippet}

		{#if wide}
			<table class="al-table" data-testid="alerts-table">
				<caption class="sr-only">{m.alerts_list_label()}</caption>
				<thead>
					<tr>
						<th scope="col">{m.alerts_col_name()}</th>
						<th scope="col">{m.alerts_col_query()}</th>
						<th scope="col" class="al-right">{m.alerts_col_new()}</th>
						<th scope="col">{m.alerts_col_channels()}</th>
						<th scope="col">{m.alerts_col_last()}</th>
						<th scope="col"><span class="sr-only">{m.alerts_col_actions()}</span></th>
					</tr>
				</thead>
				<tbody>
					{#each list.items as s (s.id)}
						<tr data-testid="alerts-row-{s.id}">
							<th scope="row">
								{@render name(s)}
								{@render status(s)}
							</th>
							<td>{@render queryText(s)}</td>
							<td class="al-right">{@render unread(s)}</td>
							<td><ChannelBadges search={s} /></td>
							<td>{@render last(s)}</td>
							<td>{@render actions(s)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{:else}
			<ul class="al-stack" aria-label={m.alerts_list_label()} data-testid="alerts-stack">
				{#each list.items as s (s.id)}
					<li class="al-card" data-testid="alerts-row-{s.id}">
						<div class="al-card-head">
							{@render name(s)}
							{@render status(s)}
						</div>
						{@render queryText(s)}
						<dl class="al-meta">
							<div>
								<dt>{m.alerts_col_new()}</dt>
								<dd>{@render unread(s)}</dd>
							</div>
							<div>
								<dt>{m.alerts_col_last()}</dt>
								<dd>{@render last(s)}</dd>
							</div>
							<div>
								<dt>{m.alerts_col_channels()}</dt>
								<dd><ChannelBadges search={s} /></dd>
							</div>
						</dl>
						{@render actions(s)}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</section>

{#if SaveDialog}
	<SaveDialog bind:open={saveOpen} />
{/if}
{#if DeleteDialog && toDelete}
	<DeleteDialog bind:open={deleteOpen} search={toDelete} onDeleted={deleted} />
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
	.al-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: calc(var(--spacing) * 3);
	}
	.al-h1 {
		margin: 0;
		font-size: var(--text-display);
		line-height: var(--text-display--line-height);
		letter-spacing: var(--text-display--letter-spacing);
		font-weight: var(--font-weight-semibold);
	}
	.al-skeleton {
		min-block-size: calc(var(--row-height-comfortable) * 3 + var(--spacing) * 4);
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
	}
	.al-page :global(.al-skeleton-row) {
		block-size: var(--row-height-comfortable);
	}
	.al-muted {
		color: var(--color-muted-text);
	}
	.al-name {
		display: inline-flex;
		align-items: center;
		font-weight: var(--font-weight-medium);
		overflow-wrap: anywhere;
		text-decoration: underline;
		text-underline-offset: calc(var(--spacing) * 1);
	}
	@media (pointer: coarse) {
		.al-name {
			min-block-size: var(--target-min);
		}
	}
	.al-num {
		font-variant-numeric: tabular-nums;
	}
	.al-right {
		text-align: end;
	}
	.al-actions {
		display: inline-flex;
		flex-wrap: wrap;
		align-items: flex-start;
		gap: calc(var(--spacing) * 3);
	}

	.al-table {
		inline-size: 100%;
		border-spacing: 0;
		text-align: start;
	}
	.al-table th,
	.al-table td {
		padding: calc(var(--spacing) * 3);
		border-bottom: var(--border) solid var(--color-base-300);
		vertical-align: top;
		text-align: start;
	}
	.al-table th[scope='col'] {
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		font-weight: var(--font-weight-medium);
	}
	.al-table th.al-right,
	.al-table td.al-right {
		text-align: end;
	}
	.al-table th[scope='row'] {
		font-weight: var(--font-weight-normal);
	}

	.al-stack {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 3);
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.al-card {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
		padding: calc(var(--spacing) * 3);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-box);
		background: var(--color-base-100);
	}
	.al-card-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: calc(var(--spacing) * 2);
	}
	.al-meta {
		display: flex;
		flex-wrap: wrap;
		gap: calc(var(--spacing) * 2) calc(var(--spacing) * 6);
		margin: 0;
	}
	.al-meta dt {
		color: var(--color-muted-text);
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
	}
	.al-meta dd {
		margin: 0;
	}
</style>
