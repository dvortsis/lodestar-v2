<script lang="ts">
	// §10.5.3 parsed metadata. Only what the server serves for one torrent: the resolved title with
	// its provenance, the type, and the title identity's spans (from siblings, fetched when this
	// section is mounted, on first scroll near). Resolution, codecs, languages, group and edition are not
	// served yet, so they have no rows rather than a "—" that would claim they were measured.
	import { showBidi } from '$lib/util/bidi';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import type { SearchHit } from '../cache';
	import { loadSiblings } from '../siblings';
	import { contentTypeLabel, resolvedVia } from './labels';
	import { joinSpans, type Identity } from './spans';

	interface Props {
		hash: string;
		hit: SearchHit | null;
	}
	let { hash, hit }: Props = $props();

	let identity = $state<Identity | null>(null);
	let status = $state<'loading' | 'ready' | 'error' | 'withheld'>('loading');
	let attempt = $state(0);

	// Mounted when the section first scrolls near; a new torrent in the same drawer starts over. The
	// request is Related's too (siblings.ts); a Retry asks again.
	$effect(() => {
		identity = null;
		return loadSiblings(
			hash,
			(s) => {
				if (s.status === 'ready') identity = s.siblings.identity;
				status = s.status === 'failed' ? 'error' : s.status;
			},
			attempt > 0
		);
	});

	const pick = (v: string[], one: () => string, many: () => string) =>
		(v.length === 1 && !v[0]!.includes('-') ? one : many)();
	const rows = $derived(
		identity
			? (
					[
						[
							pick(identity.years, m.search_filters_year, m.detail_parsed_years),
							joinSpans(identity.years),
							'years'
						],
						[
							pick(identity.seasons, m.search_filters_season, m.detail_parsed_seasons),
							joinSpans(identity.seasons),
							'seasons'
						],
						[m.detail_parsed_episodes(), joinSpans(identity.episodes), 'episodes'],
						[m.detail_parsed_absolute(), joinSpans(identity.absolute), 'absolute']
					] as const
				).filter((r) => r[1] !== null)
			: []
	);
	const type = $derived(hit?.content_type ? contentTypeLabel(hit.content_type) : null);
	const adult = $derived(hit?.adult_kind ? contentTypeLabel(hit.adult_kind, 'adult') : null);
</script>

<div class="ls-detail-stack" data-testid="detail-parsed">
	<dl class="ls-detail-facts">
		<dt>{m.resolved_title()}</dt>
		<dd data-testid="detail-parsed-title">
			{#if hit?.title}
				<span>{showBidi(hit.title)}</span>
				<span class="block text-small text-muted-text" data-testid="detail-parsed-provenance"
					>{hit.resolved_via
						? m.detail_resolved_via({ method: resolvedVia(hit.resolved_via) })
						: m.detail_from_name()}</span
				>
			{:else}
				{m.detail_parsed_none()}
			{/if}
		</dd>
		<dt>{m.content_type()}</dt>
		<dd data-testid="detail-parsed-type">
			{#if type}{type}{#if adult}, {adult}{/if}{:else}{m.detail_parsed_none()}{/if}
		</dd>
		{#each rows as [name, value, key] (key)}
			<dt>{name}</dt>
			<dd class="tabular-nums" data-testid="detail-parsed-{key}">{value}</dd>
		{/each}
	</dl>
	{#if status === 'error'}
		<p class="text-muted-text" role="status">{m.detail_parsed_error()}</p>
		<div><Button dense onclick={() => attempt++}>{m.detail_retry()}</Button></div>
	{/if}
</div>
