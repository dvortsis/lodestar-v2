<script lang="ts">
	// Settings → Sources (§18.8, §10.10): every source plugin of this build, grouped by kind, with its
	// status, enable toggle, priority and 24 h yield. Consent dialogs, credentials and purge are
	// SRC-15b. This screen shows the server's last answer and replaces a row with the PATCH's reply.
	import { onDestroy, onMount } from 'svelte';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import SourceRow from './SourceRow.svelte';
	import {
		group,
		kindText,
		refusalText,
		type Priority,
		type Source,
		type SourcePatch
	} from './sources';

	let sources = $state.raw<Source[] | null>(null);
	let failed = $state(false);
	let problem = $state('');
	let busy = $state.raw<readonly number[]>([]);
	let ctl: AbortController | undefined;
	let destroyed = false;

	const groups = $derived(sources ? group(sources) : []);

	async function load() {
		ctl?.abort();
		const mine = (ctl = new AbortController());
		try {
			sources = (await api.get('/sources', { signal: mine.signal })).sources;
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}

	const mark = (id: number, on: boolean) =>
		(busy = on ? [...busy, id] : busy.filter((x) => x !== id));

	/** Sends one change; the reply replaces the row. A refusal is shown and the list reloads. */
	async function change(s: Source, body: SourcePatch) {
		if (busy.includes(s.id)) return;
		problem = '';
		mark(s.id, true);
		try {
			const updated = await api.patch('/sources/{id}', { path: { id: s.id }, body });
			if (destroyed) return;
			sources = sources!.map((x) => (x.id === s.id ? updated : x));
		} catch (e) {
			if (destroyed) return;
			problem = refusalText(e);
			await load();
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

{#if problem}
	<div role="alert" data-testid="sources-problem"><Banner kind="error">{problem}</Banner></div>
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
		<div class="flex flex-col gap-8" data-testid="sources-screen">
			{#each groups as g (g.kind ?? 'other')}
				<section class="flex flex-col gap-3" aria-labelledby="sources-{g.kind ?? 'other'}-h">
					<h2 id="sources-{g.kind ?? 'other'}-h" class="text-small font-semibold">
						{kindText(g.kind)}
					</h2>
					<ul class="flex flex-col gap-3" data-testid="sources-group-{g.kind ?? 'other'}">
						{#each g.sources as s (s.id)}
							<SourceRow
								source={s}
								busy={busy.includes(s.id)}
								onenable={(enabled) => change(s, { enabled })}
								onpriority={(priority: Priority) => change(s, { priority })}
							/>
						{/each}
					</ul>
				</section>
			{/each}
		</div>
	{/if}
{:else if failed}
	<div role="alert" data-testid="sources-failed">
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
