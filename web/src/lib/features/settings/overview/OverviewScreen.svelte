<script lang="ts">
	// Settings → Overview (§10.10.3): "is anything wrong, and what did I change?". Loaded on demand
	// by the route. Reads `/system`, the network check and every section's values; the decisions
	// are in `logic.ts`.
	import { resolve } from '$app/paths';
	import type { Route } from '$lib/shell/nav';
	import { api, isAbort } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { displayValue } from '../form/display';
	import { fieldLabel, loadMessages } from '../form/messages';
	import { loadSettingsIndex } from '../form/schema';
	import { getSection } from '../form/requests';
	import type { SectionValues } from '../form/types';
	import { screens } from '../shell/screens';
	import {
		changedFromDefault,
		needsAttention,
		type Attention,
		type ChangedGroup,
		type ChangedRow
	} from './logic';

	interface Loaded {
		attention: Attention[] | undefined;
		groups: ChangedGroup[];
	}
	let data = $state<Loaded>();
	let failed = $state(false);
	let attempt = $state(0);

	const screenOf = (section: string) =>
		screens.find((s) => s.sections?.includes(section))?.id ??
		screens.find((s) => s.id === section)?.id;
	const titleOf = (id: string) => screens.find((s) => s.id === id)?.title() ?? id;

	$effect(() => {
		void attempt;
		const ctl = new AbortController();
		failed = false;
		data = undefined;
		(async () => {
			const index = await loadSettingsIndex();
			const names = index.sections.map((s) => s.section);
			// The health parts are best-effort: a failure there leaves "unavailable", not a blank page.
			const [values, system, network] = await Promise.all([
				Promise.all(names.map((s) => getSection(s, ctl.signal))),
				api.get('/system', { signal: ctl.signal }).catch(() => undefined),
				api.get('/network/check', { signal: ctl.signal }).catch(() => undefined),
				loadMessages(names)
			]);
			data = {
				attention: system && needsAttention(system, network),
				groups: changedFromDefault((key) => index.spec(key), values as SectionValues[], screenOf)
			};
		})().catch((e: unknown) => {
			if (!isAbort(e)) failed = true;
		});
		return () => ctl.abort();
	});

	const href = (path: string) => resolve(path as Route);

	function sentence(a: Attention): string {
		switch (a.kind) {
			case 'rebuild_failed':
				return m.settings_overview_rebuild_failed();
			case 'address_pending':
				return m.settings_overview_address_pending();
			case 'filesystem':
				return m.settings_overview_filesystem({ detail: a.detail ?? '' });
			case 'unreachable':
				return m.settings_overview_unreachable();
			case 'restart':
				return m.settings_overview_restart({ count: a.count ?? 0 });
			case 'errors':
				return m.settings_overview_errors({ count: a.count ?? 0 });
			case 'rebuilding':
				return m.settings_overview_rebuilding();
		}
	}

	function sourceText(r: ChangedRow): string {
		if (r.source === 'db') return m.settings_overview_source_db();
		if (r.source === 'file') return m.settings_overview_source_file();
		return r.env
			? m.settings_overview_source_env({ env: r.env })
			: m.settings_overview_source_env_unnamed();
	}
</script>

{#if failed}
	<Banner kind="error">
		{m.settings_overview_failed()}
		{#snippet action()}
			<Button onclick={() => attempt++}>{m.settings_overview_retry()}</Button>
		{/snippet}
	</Banner>
{:else if !data}
	<p role="status" class="text-muted-text">{m.settings_overview_loading()}</p>
{:else}
	<section aria-labelledby="overview-attention" class="flex flex-col gap-3">
		<h2 id="overview-attention" class="text-heading font-semibold">
			{m.settings_overview_attention()}
		</h2>
		{#if !data.attention}
			<p class="text-muted-text">{m.settings_overview_attention_unavailable()}</p>
		{:else if data.attention.length === 0}
			<p data-testid="attention-empty">{m.settings_overview_attention_empty()}</p>
		{:else}
			<ul class="flex flex-col gap-2" data-testid="attention-list">
				{#each data.attention as a (a.kind)}
					<li>
						<Banner kind={a.severity === 'info' ? 'info' : a.severity}>
							{sentence(a)}
							{#snippet action()}
								<a class="ls-focus text-primary underline" href={href(`/settings/${a.screen}`)}
									>{m.settings_overview_go({ screen: titleOf(a.screen) })}</a
								>
							{/snippet}
						</Banner>
					</li>
				{/each}
			</ul>
		{/if}
	</section>

	<section aria-labelledby="overview-changed" class="mt-6 flex flex-col gap-3">
		<h2 id="overview-changed" class="text-heading font-semibold">
			{m.settings_overview_changed()}
		</h2>
		{#if data.groups.length === 0}
			<p data-testid="changed-empty">{m.settings_overview_changed_empty()}</p>
		{:else}
			{#each data.groups as g (g.id)}
				<section aria-labelledby="changed-{g.id}" class="flex flex-col gap-1">
					<h3 id="changed-{g.id}" class="font-medium">
						{#if g.linked}
							<a class="ls-focus text-primary underline" href={href(`/settings/${g.id}`)}
								>{titleOf(g.id)}</a
							>
						{:else}
							{titleOf(g.id)}
						{/if}
						<span class="font-normal text-muted-text"
							>· {m.settings_overview_changed_count({ count: g.rows.length })}</span
						>
					</h3>
					<ul class="flex flex-col divide-y divide-field-border">
						{#each g.rows as r (r.spec.key)}
							<li
								class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-2"
								data-testid="changed-row"
							>
								{#if g.linked}
									<a
										class="ls-focus text-primary underline"
										href={href(`/settings/${g.id}#${r.spec.key}`)}>{fieldLabel(r.spec)}</a
									>
								{:else}
									<span>{fieldLabel(r.spec)}</span>
								{/if}
								<span class="min-w-0 break-words">
									<span class="font-mono"
										>{r.secret
											? m.settings_overview_secret_set()
											: displayValue(r.spec, r.value)}</span
									>
									<span class="text-small text-muted-text">· {sourceText(r)}</span>
								</span>
							</li>
						{/each}
					</ul>
				</section>
			{/each}
		{/if}
	</section>
{/if}
