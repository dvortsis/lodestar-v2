<script lang="ts">
	// Settings → Filters & Safety → Blocklist subscriptions (§20.13, owner §2.9): the one-click
	// catalog, your lists with their figures, pause, resume, remove and the held-version choice.
	// Nothing is subscribed by default. Every change goes through the filters flows' dialog (preview,
	// strictness, effect warning, password); a weakening change always shows the server's exact
	// counts and asks for the password there. This screen shows the server's last answer and
	// reloads it once a change applied.
	import { onDestroy, onMount, type Component } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { formatCount, formatDateTime, formatPercent } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import type { ChangeFlow } from '../filters/flows/flow.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import {
		connect,
		pause,
		refusalText,
		remove,
		settle,
		type Change,
		type StrictnessList,
		type Subscriptions
	} from './lists';
	import StrictnessNote from './StrictnessNote.svelte';

	let data = $state.raw<Subscriptions | null>(null);
	let failed = $state(false);
	let flow = $state.raw<ChangeFlow | null>(null);
	let Dialog = $state<Component<Record<string, unknown>>>();
	let pending = $state({ title: '', applyLabel: '' });
	let problem = $state('');
	let heading = $state<HTMLElement>();
	let ctl: AbortController | undefined;
	let creating: Promise<ChangeFlow> | undefined;
	let destroyed = false;

	const locale = () => currentLocale();

	async function load() {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		try {
			data = await api.get('/filters/subscriptions', { signal: mine.signal });
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}

	/** Previews `change` in the flows' dialog; nothing is sent until the dialog says so. */
	async function ask(change: Change, title: string, applyLabel: string) {
		problem = '';
		try {
			// The flows load through their one door, on the first change (S-filters-2c).
			creating ??= import('../filters/flows-door')
				.then((door) => door.load())
				.then((flows) => {
					Dialog = flows.ChangeDialog as never;
					return flows.createChangeFlow();
				});
			const f = await creating;
			if (destroyed) return;
			flow = f;
			if (f.state.kind !== 'applying') pending = { title, applyLabel };
			f.start(change);
		} catch (e) {
			creating = undefined;
			problem = refusalText(e);
		}
	}

	// A change can remove the row whose button opened the dialog: focus falls to the heading.
	function keepFocus() {
		requestAnimationFrame(() =>
			requestAnimationFrame(() => {
				if (!document.activeElement || document.activeElement === document.body) heading?.focus();
			})
		);
	}

	const figures = (f: StrictnessList) =>
		f.kind === 'ip'
			? m.blocklists_figures_ip({ share: formatPercent(f.ipv4_public_share, locale()) })
			: m.blocklists_figures_content({ hashes: formatCount(f.hashes, locale()) });

	onMount(load);
	onDestroy(() => {
		destroyed = true;
		ctl?.abort();
		// An apply already sent can't be cancelled: let its answer go.
		if (flow && flow.state.kind !== 'applying') flow.cancel();
	});
</script>

<h1 class="text-title font-semibold" tabindex="-1" bind:this={heading}>{m.blocklists_title()}</h1>
<p class="text-muted-text">{m.blocklists_purpose()}</p>

{#if problem}
	<div role="alert" data-testid="blocklists-problem"><Banner kind="error">{problem}</Banner></div>
{/if}

{#if data}
	{@const d = data}
	{#if failed}
		<div data-testid="blocklists-stale">
			<Banner kind="warning">
				{m.blocklists_stale()}
				{#snippet action()}<Button dense onclick={load}>{m.blocklists_retry()}</Button>{/snippet}
			</Banner>
		</div>
	{/if}
	<div class="flex flex-col gap-8" data-testid="blocklists-screen">
		<section class="flex flex-col gap-3" aria-labelledby="blocklists-mine-h">
			<h2 id="blocklists-mine-h" class="text-small font-semibold">{m.blocklists_mine_heading()}</h2>
			{#if d.connected.length === 0}
				<EmptyState
					headingLevel={3}
					title={m.blocklists_empty_title()}
					text={m.blocklists_empty_text()}
					data-testid="blocklists-empty"
				/>
			{:else}
				<StrictnessNote strictness={d.strictness} />
				<ul class="flex flex-col gap-3" data-testid="blocklists-mine">
					{#each d.connected as c (c.subscription.id)}
						{@const s = c.subscription}
						<li
							class="flex flex-col gap-2 rounded-box border border-field-border p-4"
							data-testid="list-{s.id}"
						>
							<div class="flex flex-wrap items-center gap-2">
								<h3 class="text-body font-semibold">{s.name}</h3>
								<Badge variant={s.enabled ? 'success' : 'warning'}
									>{s.enabled ? m.blocklists_state_active() : m.blocklists_state_paused()}</Badge
								>
								{#if c.held}
									<Badge variant="warning"
										><span data-testid="list-{s.id}-held">{m.blocklists_state_held()}</span></Badge
									>
								{/if}
								{#if s.floor}<Badge>{m.blocklists_state_floor()}</Badge>{/if}
							</div>
							<p class="text-small text-muted-text" data-testid="list-{s.id}-figures">
								{figures(c.figures)}{#if c.figures.measured}
									· {m.blocklists_measured()}{/if}{#if s.updated_at !== null}
									· {m.blocklists_updated({ when: formatDateTime(s.updated_at, locale()) })}{/if}
							</p>
							{#if s.error}
								<p class="text-small text-error" data-testid="list-{s.id}-error">
									{m.blocklists_list_error({ error: s.error })}
								</p>
							{/if}
							{#if c.held}
								<p class="text-small" data-testid="list-{s.id}-held-text">
									{m.blocklists_held_text()}
								</p>
							{/if}
							<div class="flex flex-wrap gap-2">
								{#if c.held}
									<Button
										dense
										onclick={() =>
											ask(
												settle(s.id, true),
												m.blocklists_accept_title({ name: s.name }),
												m.blocklists_accept()
											)}
										data-testid="list-{s.id}-accept">{m.blocklists_accept()}</Button
									>
									<Button
										dense
										onclick={() =>
											ask(
												settle(s.id, false),
												m.blocklists_keep_title({ name: s.name }),
												m.blocklists_keep()
											)}
										data-testid="list-{s.id}-keep">{m.blocklists_keep()}</Button
									>
								{/if}
								<Button
									dense
									onclick={() =>
										ask(
											pause(s.id, !s.enabled),
											s.enabled
												? m.blocklists_pause_title({ name: s.name })
												: m.blocklists_resume_title({ name: s.name }),
											s.enabled ? m.blocklists_pause() : m.blocklists_resume()
										)}
									data-testid="list-{s.id}-pause"
									>{s.enabled ? m.blocklists_pause() : m.blocklists_resume()}</Button
								>
								<Button
									dense
									onclick={() =>
										ask(
											remove(s.id),
											m.blocklists_remove_title({ name: s.name }),
											m.blocklists_remove()
										)}
									data-testid="list-{s.id}-remove">{m.blocklists_remove()}</Button
								>
							</div>
						</li>
					{/each}
				</ul>
			{/if}
		</section>

		<section class="flex flex-col gap-3" aria-labelledby="blocklists-catalog-h">
			<h2 id="blocklists-catalog-h" class="text-small font-semibold">
				{m.blocklists_catalog_heading()}
			</h2>
			<p class="text-muted-text">{m.blocklists_catalog_help()}</p>
			<ul class="flex flex-col gap-3" data-testid="blocklists-catalog">
				{#each d.catalog as l (l.key)}
					<li
						class="flex flex-col gap-2 rounded-box border border-field-border p-4"
						data-testid="catalog-{l.key}"
					>
						<div class="flex flex-wrap items-center gap-2">
							<h3 class="text-body font-semibold">{l.name}</h3>
							<Badge>{l.kind === 'ip' ? m.blocklists_kind_ip() : m.blocklists_kind_content()}</Badge
							>
						</div>
						<p>{l.about}</p>
						<p class="text-small text-muted-text">
							{figures(l.figures)} · {m.blocklists_measured()}
						</p>
						<div>
							{#if l.subscription !== null}
								<Badge variant="success">{m.blocklists_connected()}</Badge>
							{:else}
								<Button
									dense
									onclick={() =>
										ask(
											connect(l),
											m.blocklists_connect_title({ name: l.name }),
											m.blocklists_connect()
										)}
									data-testid="catalog-{l.key}-connect">{m.blocklists_connect()}</Button
								>
							{/if}
						</div>
					</li>
				{/each}
			</ul>
		</section>
	</div>
{:else if failed}
	<div data-testid="blocklists-failed">
		<Banner kind="error">
			{m.blocklists_load_failed()}
			{#snippet action()}<Button dense onclick={load} data-testid="blocklists-retry"
					>{m.blocklists_retry()}</Button
				>{/snippet}
		</Banner>
	</div>
{:else}
	<p role="status" class="sr-only">{m.blocklists_loading()}</p>
	<Skeleton lines={6} />
{/if}

{#if Dialog && flow}
	<Dialog
		{flow}
		title={pending.title}
		applyLabel={pending.applyLabel}
		onapplied={load}
		onclose={keepFocus}
	>
		{#snippet extra()}
			{@const st = flow!.state}
			{#if (st.kind === 'review' || st.kind === 'confirm' || st.kind === 'applying') && st.assessment.strictness}
				<StrictnessNote strictness={st.assessment.strictness} />
			{/if}
		{/snippet}
	</Dialog>
{/if}
