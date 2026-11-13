<script lang="ts">
	// Settings → Sources (§18.8, §10.10): every source plugin of this build, grouped by kind, with its
	// status, enable toggle, priority and 24 h yield. Groups collapse behind a summary line, a filter
	// box narrows the rows by name or kind, each row is one line that expands, and a legend names
	// the consent classes (X-sources-restructure, owner §2.66). The consent, purge and settings
	// dialogs (SRC-15b) load on first use; until a dialog's chunk arrives the screen stays as it is.
	// The screen shows the server's last answer and replaces a row with the PATCH's reply.
	import { onDestroy, onMount, tick } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import IconCaret from '~icons/ph/caret-right-bold';
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import Legend from './Legend.svelte';
	import SourceRow from './SourceRow.svelte';
	import {
		group,
		kindText,
		matches,
		refusalText,
		summary,
		type Priority,
		type Source,
		type SourcePatch
	} from './sources';

	let sources = $state.raw<Source[] | null>(null);
	let failed = $state(false);
	let problem = $state('');
	let announce = $state('');
	let busy = $state.raw<readonly number[]>([]);
	let ctl: AbortController | undefined;
	let destroyed = false;
	// Counts the PATCH replies taken in: a list read that started before the latest reply is older
	// than it, so its answer is dropped instead of undoing that change.
	let replies = 0;
	type Dialog = { kind: 'consent' | 'purge' | 'config'; id: number } | null;
	let dialog = $state.raw<Dialog>(null);
	let dialogOpen = $state(false);
	const target = $derived(dialog ? (sources?.find((x) => x.id === dialog!.id) ?? null) : null);
	// Each dialog's code is fetched on first use; until it arrives the screen stays as it is.
	let ConsentDialog = $state.raw<typeof import('./ConsentDialog.svelte').default | null>(null);
	let PurgeDialog = $state.raw<typeof import('./PurgeDialog.svelte').default | null>(null);
	let ConfigDialog = $state.raw<typeof import('./ConfigDialog.svelte').default | null>(null);
	async function show(kind: NonNullable<Dialog>['kind'], s: Source) {
		try {
			const root = await import('./dialogs-root');
			if (kind === 'consent') ConsentDialog ??= await root.consentDialog();
			else if (kind === 'config') ConfigDialog ??= await root.configDialog();
			else PurgeDialog ??= await root.purgeDialog();
		} catch {
			problem = m.sources_error_network();
			return;
		}
		dialog = { kind, id: s.id };
		dialogOpen = true;
	}
	/** The enable body a dialog sends: the PATCH schema has no `password` (it is never returned). */
	type EnableBody = SourcePatch & { password?: string };

	const groups = $derived(sources ? group(sources) : []);
	// The filter box narrows the rows; groups with no match are left out.
	let query = $state('');
	const filtering = $derived(query.trim() !== '');
	const shown = $derived(
		groups
			.map((g) => ({ ...g, sources: g.sources.filter((s) => matches(s, query)) }))
			.filter((g) => g.sources.length > 0)
	);
	const shownCount = $derived(shown.reduce((n, g) => n + g.sources.length, 0));
	// Groups start open; a closed one opens again when the filter changes, so a match is never
	// hidden behind a closed group.
	let closed = $state.raw<readonly string[]>([]);
	const keyOf = (g: { kind: string | null }) => g.kind ?? 'other';
	const toggleGroup = (key: string) =>
		(closed = closed.includes(key) ? closed.filter((k) => k !== key) : [...closed, key]);
	function filtered(e: Event & { currentTarget: HTMLInputElement }) {
		query = e.currentTarget.value;
		closed = [];
	}

	async function load() {
		if (destroyed) return;
		ctl?.abort();
		const mine = (ctl = new AbortController());
		const seen = replies;
		try {
			const list = (await api.get('/sources', { signal: mine.signal })).sources;
			if (seen === replies) sources = list;
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}

	/** A companion a source waited for was installed: say so, then read the sources again. */
	async function refreshed(note?: string) {
		if (note) announce = note;
		await load();
	}

	const mark = (id: number, on: boolean) =>
		(busy = on ? [...busy, id] : busy.filter((x) => x !== id));

	/** Sends one change and takes the reply as the row; throws what the server refused with. */
	async function apply(s: Source, body: EnableBody): Promise<void> {
		const updated = await api.patch('/sources/{id}', { path: { id: s.id }, body });
		if (destroyed) return;
		replies++;
		sources = sources!.map((x) => (x.id === s.id ? updated : x));
		announce = m.sources_saved({ name: s.name });
	}

	/** Sends one change; the reply replaces the row. Resolves to whether the server took it. */
	async function change(s: Source, body: SourcePatch): Promise<boolean> {
		if (busy.includes(s.id)) return false;
		problem = '';
		announce = '';
		mark(s.id, true);
		try {
			await apply(s, body);
			return true;
		} catch (e) {
			if (destroyed) return false;
			problem = refusalText(e);
			await load();
			return false;
		} finally {
			mark(s.id, false);
		}
	}

	onMount(load);
	onDestroy(() => {
		destroyed = true;
		ctl?.abort();
	});
</script>

<h1 class="text-title font-semibold" tabindex="-1">{m.sources_title()}</h1>
<p class="text-muted-text">{m.sources_purpose()}</p>
<p class="sr-only" role="status" data-testid="sources-announce">{announce}</p>

{#if problem}
	<div data-testid="sources-problem"><Banner kind="error">{problem}</Banner></div>
{/if}

{#if target && dialog}
	{#if dialog.kind === 'consent' && ConsentDialog && target.consent !== 'none'}
		<ConsentDialog
			bind:open={dialogOpen}
			source={target}
			onenable={(body) => apply(target, { enabled: true, ...body })}
			onstale={load}
			onsaved={load}
		/>
	{:else if dialog.kind === 'config' && ConfigDialog}
		{#key target.id}
			<ConfigDialog
				bind:open={dialogOpen}
				source={target}
				onsave={(config) => apply(target, { config: config as Record<string, never> })}
			/>
		{/key}
	{:else if dialog.kind === 'purge' && PurgeDialog}
		{#key target.id}
			<PurgeDialog bind:open={dialogOpen} source={target} onfinished={load} />
		{/key}
	{/if}
{/if}

{#if sources}
	{#if failed}
		<div data-testid="sources-stale">
			<Banner kind="warning">
				{m.sources_stale()}
				{#snippet action()}<Button dense onclick={load}>{m.sources_retry()}</Button>{/snippet}
			</Banner>
		</div>
	{/if}
	{#if groups.length === 0}
		<EmptyState
			headingLevel={2}
			title={m.sources_empty_title()}
			text={m.sources_empty_text()}
			data-testid="sources-empty"
		/>
	{:else}
		<div class="flex flex-col gap-6" data-testid="sources-screen">
			<div class="flex flex-col gap-2">
				<Input
					type="search"
					class="max-w-md"
					id="sources-filter"
					aria-label={m.sources_filter_label()}
					placeholder={m.sources_filter_placeholder()}
					value={query}
					oninput={filtered}
					data-testid="sources-filter"
				/>
				<p class="text-small text-muted-text" role="status" data-testid="sources-filter-count">
					{filtering
						? sources.length === 1
							? m.sources_filter_count_one({ shown: shownCount })
							: m.sources_filter_count({ shown: shownCount, total: sources.length })
						: ''}
				</p>
			</div>
			{#if shown.length === 0}
				<EmptyState
					headingLevel={2}
					title={m.sources_filter_empty_title()}
					text={m.sources_filter_empty_text()}
					data-testid="sources-filter-empty"
				>
					{#snippet action()}
						<Button
							onclick={() => {
								query = '';
								closed = [];
								// The button goes with the empty state: put the keyboard back in the filter.
								void tick().then(() => document.getElementById('sources-filter')?.focus());
							}}
							data-testid="sources-filter-clear">{m.sources_filter_clear()}</Button
						>
					{/snippet}
				</EmptyState>
			{/if}
			{#each shown as g (keyOf(g))}
				{@const key = keyOf(g)}
				{@const open = !closed.includes(key)}
				{@const sum = summary(g.sources)}
				<section class="flex flex-col gap-3" aria-labelledby="sources-{key}-h">
					<h2 id="sources-{key}-h" class="text-small">
						<button
							type="button"
							class="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-field text-start {FOCUS_RING} {TARGET_MIN}"
							aria-expanded={open}
							aria-controls="sources-group-{key}"
							onclick={() => toggleGroup(key)}
							data-testid="sources-group-{key}-toggle"
						>
							<IconCaret
								class="size-(--icon-size-row) shrink-0 {open ? 'rotate-90' : ''}"
								aria-hidden="true"
							/>
							<span class="font-semibold">{kindText(g.kind)}</span>
							<span class="text-muted-text" data-testid="sources-group-{key}-summary">
								{m.sources_group_on({ on: sum.on, total: sum.total })}
							</span>
							{#if sum.attention > 0}
								<span data-testid="sources-group-{key}-attention">
									{m.sources_group_attention({ count: sum.attention })}
								</span>
							{/if}
						</button>
					</h2>
					<ul
						id="sources-group-{key}"
						class="flex flex-col gap-3"
						hidden={!open}
						data-testid="sources-group-{key}"
					>
						{#each g.sources as s (s.id)}
							<SourceRow
								source={s}
								busy={busy.includes(s.id)}
								onenable={(enabled) => change(s, { enabled })}
								onpriority={(priority: Priority) => change(s, { priority })}
								onsetup={() => show('consent', s)}
								onpurge={() => show('purge', s)}
								onsettings={() => show('config', s)}
								onrefresh={refreshed}
							/>
						{/each}
					</ul>
				</section>
			{/each}
			<Legend />
		</div>
	{/if}
{:else if failed}
	<div data-testid="sources-failed">
		<Banner kind="error">
			{m.sources_load_failed()}
			{#snippet action()}<Button dense onclick={load}>{m.sources_retry()}</Button>{/snippet}
		</Banner>
	</div>
{:else}
	<div aria-busy="true" aria-label={m.sources_loading()} data-testid="sources-loading">
		<Skeleton lines={4} />
	</div>
{/if}
