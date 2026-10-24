<script lang="ts">
	// The facet rail (§10.4): the server's facets for the shown query, in decision order, then
	// "More filters". Counts are the server's (disjunctive, "≈" when sampled, one note per panel);
	// every click is an AST edit (toggle.ts) handed to `onchange`. The host decides what a change
	// does: apply it at once (rail, left sheet) or stage it (bottom sheet).
	import Info from '~icons/ph/info';
	import { m } from '$lib/paraglide/messages.js';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import type { ApiError } from '$lib/api/client';
	import type { Ast } from '$lib/state/search.svelte';
	import type { FacetsResponse, TermFacet } from './facets.svelte';
	import { wholeDays } from './bins';
	import RangeGroup from './RangeGroup.svelte';
	import SwarmGroup from './SwarmGroup.svelte';
	import TermGroup from './TermGroup.svelte';
	import {
		clearField,
		dateOf,
		fieldOf,
		fixedValues,
		rangeOf,
		setDate,
		setRange,
		toggleValue,
		type When
	} from './toggle';
	import './facets.css';

	interface Props {
		data: FacetsResponse | null;
		/** The query the counts are for: what a click edits. */
		ast: Ast | null;
		error: ApiError | null;
		/** Facets showing up to 100 values ("Show more"). */
		more: ReadonlySet<string>;
		onmore: (facet: string, on: boolean) => void;
		onchange: (next: Ast) => void;
		onretry: () => void;
	}
	let { data, ast, error, more, onmore, onchange, onretry }: Props = $props();

	const DAY = 86_400;
	const PRESETS: [number, () => string][] = [
		[DAY, m.search_facets_last_24h],
		[7 * DAY, m.search_facets_last_7d],
		[30 * DAY, m.search_facets_last_30d],
		[365 * DAY, m.search_facets_last_1y]
	];

	const TITLE: Record<string, () => string> = {
		type: m.search_facets_type,
		ext: m.search_facets_ext,
		res: m.search_facets_res,
		lang: m.search_facets_lang,
		group: m.search_facets_group,
		codec: m.search_facets_codec,
		audio: m.search_facets_audio,
		hdr: m.search_facets_hdr,
		private: m.search_facets_private,
		v2: m.search_facets_v2,
		has: m.search_facets_has,
		ambiguous: m.search_facets_ambiguous,
		imported: m.search_facets_imported,
		backfill: m.search_facets_backfill,
		adult: m.search_facets_adult
	};
	const TYPE: Record<string, () => string> = {
		movie: m.content_type_movie,
		tv: m.content_type_tv,
		music: m.content_type_music,
		audiobook: m.content_type_audiobook,
		ebook: m.content_type_ebook,
		comic: m.content_type_comic,
		software: m.content_type_software,
		game: m.content_type_game,
		images: m.content_type_images,
		video: m.content_type_video,
		other: m.content_type_other,
		adult: m.content_type_adult
	};
	const FLAGS = new Set(['private', 'v2', 'ambiguous', 'imported', 'backfill']);
	const MORE = [
		'lang',
		'group',
		'codec',
		'audio',
		'hdr',
		'private',
		'v2',
		'has',
		'ambiguous',
		'imported',
		'backfill',
		'adult'
	];
	const yesNo = (v: string) =>
		v === 'yes' ? m.search_facets_yes() : v === 'no' ? m.search_facets_no() : v;
	const labelFor = (name: string) =>
		name === 'type' ? (v: string) => TYPE[v]?.() ?? v : FLAGS.has(name) ? yesNo : (v: string) => v;

	const facet = (name: string): TermFacet | undefined => data?.facets[name];
	const blocked = (name: string) => !!data?.unavailable.includes(name);
	const moreShown = $derived(MORE.filter((n) => facet(n) || blocked(n)));

	function value(name: string, v: string, how: 'include' | 'exclude') {
		const field = fieldOf(name);
		if (field) onchange(toggleValue(ast, field, v, how));
	}
	const NONE: ReadonlySet<string> = new Set();
	function fixedOf(name: string) {
		const field = fieldOf(name);
		return field ? fixedValues(ast, field) : NONE;
	}
	function clear(name: string) {
		const field = fieldOf(name);
		if (field) onchange(clearField(ast, field));
	}

	// Discovered: presets are "newer than N ago"; a brush is a fixed span of days.
	const now = () => Math.floor(Date.now() / 1000);
	const discovered = $derived(dateOf(ast, 'discovered'));
	const preset = $derived(
		discovered?.to === null && discovered.from && 'ago' in discovered.from
			? discovered.from.ago
			: null
	);
	const at = (w: When | null, t: number) => (w === null ? null : 'at' in w ? w.at : t - w.ago);
	const discoveredRange = $derived(
		discovered ? { lo: at(discovered.from, now()), hi: at(discovered.to, now()) } : null
	);
	const sizeRange = $derived(rangeOf(ast, 'size'));
	const atLeast = (field: 'seeders' | 'trackers') => {
		const r = rangeOf(ast, field);
		return r && r.hi === null ? r.lo : null;
	};
	let histogram = $state<HTMLElement>();
	let custom = $state(false);
</script>

{#snippet term(name: string)}
	{@const f = facet(name)}
	<!-- A facet with no values for this query says nothing (the server lists every one it serves). -->
	{#if f && f.values.length}
		<TermGroup
			{name}
			title={TITLE[name]?.() ?? name}
			facet={f}
			labelOf={labelFor(name)}
			more={more.has(name)}
			onmore={name === 'ext' ? (on) => onmore(name, on) : undefined}
			excludable={!FLAGS.has(name)}
			fixed={fixedOf(name)}
			onvalue={(v, how) => value(name, v, how)}
			onclear={() => clear(name)}
		/>
	{:else if blocked(name)}
		{@render unavailable(name, TITLE[name]?.() ?? name)}
	{/if}
{/snippet}

{#snippet unavailable(name: string, title: string)}
	<section class="fr-group" data-testid="facet-{name}">
		<h3 class="fr-title">{title}</h3>
		<p class="fr-muted" data-testid="facet-{name}-unavailable">
			{m.search_facets_unavailable()}. {m.search_facets_unavailable_index()}
		</p>
	</section>
{/snippet}

<div class="fr" data-testid="facet-rail">
	{#if error}
		<Banner kind="error" title={m.search_facets_error()}>
			<span data-testid="facet-error">{m.search_facets_error_body()}</span>
			{#snippet action()}
				<Button dense onclick={onretry}>{m.search_facets_retry()}</Button>
			{/snippet}
		</Banner>
	{/if}
	{#if data}
		{#if data.sampled}
			<p class="fr-note" data-testid="facet-sampled">
				<Info class="fr-note-icon" aria-hidden="true" />{m.sampled_note()}
			</p>
		{/if}
		{@render term('type')}
		{@const disc = data.histograms.discovered}
		{#if disc}
			<div bind:this={histogram}>
				<RangeGroup
					name="discovered"
					title={m.search_facets_discovered()}
					hist={disc}
					range={discoveredRange}
					onrange={(brushed) => {
						// Whole days: the clause is printed as days, so finer edges wouldn't survive the URL.
						const r = wholeDays(brushed);
						onchange(
							setDate(
								ast,
								'discovered',
								r?.lo == null ? null : { at: r.lo },
								r?.hi == null ? null : { at: r.hi }
							)
						);
					}}
				>
					{#snippet presets()}
						<div
							class="fr-presets"
							role="group"
							aria-label={m.search_facets_presets({ facet: m.search_facets_discovered() })}
						>
							{#each PRESETS as [ago, label] (ago)}
								<button
									type="button"
									class="fr-preset"
									aria-pressed={preset === ago}
									data-testid="facet-discovered-{ago}"
									onclick={() =>
										onchange(setDate(ast, 'discovered', preset === ago ? null : { ago }, null))}
									>{label()}</button
								>
							{/each}
							<!-- Custom starts a brush: it focuses the histogram and says how (never hover-only). -->
							<button
								type="button"
								class="fr-preset"
								aria-describedby={custom ? 'fr-custom-hint' : undefined}
								data-testid="facet-discovered-custom"
								onclick={() => {
									custom = true;
									histogram?.querySelector<HTMLElement>('[role="group"][tabindex]')?.focus();
								}}>{m.search_facets_custom()}</button
							>
						</div>
						{#if custom}<p class="fr-muted" id="fr-custom-hint">
								{m.search_facets_custom_hint()}
							</p>{/if}
					{/snippet}
				</RangeGroup>
			</div>
		{:else if blocked('discovered')}
			{@render unavailable('discovered', m.search_facets_discovered())}
		{/if}
		{@const size = data.histograms.size}
		{#if size}
			<RangeGroup
				name="size"
				title={m.search_facets_size()}
				hist={size}
				range={sizeRange}
				onrange={(r) => onchange(setRange(ast, 'size', r?.lo ?? null, r?.hi ?? null))}
			/>
		{:else if blocked('size')}
			{@render unavailable('size', m.search_facets_size())}
		{/if}
		{@render term('ext')}
		{@render term('res')}
		{#each ['seeders', 'trackers'] as const as name (name)}
			<SwarmGroup
				{name}
				title={name === 'seeders' ? m.search_facets_seeders() : m.search_facets_trackers()}
				bucket={data.buckets[name]}
				atLeast={atLeast(name)}
				onatleast={(n) => onchange(setRange(ast, name, n, null))}
			/>
		{/each}
		{@const year = data.histograms.year}
		{#if moreShown.length || year}
			<details class="fr-more" data-testid="facet-more">
				<summary>{m.search_facets_more_filters()}</summary>
				<div class="fr-more-body">
					{#if year}
						<!-- Release years: bins are [y, y + 1), the clause's bounds are years. -->
						<RangeGroup
							name="year"
							title={m.search_facets_year()}
							hist={year}
							range={rangeOf(ast, 'year')}
							onrange={(r) => onchange(setRange(ast, 'year', r?.lo ?? null, r?.hi ?? null))}
						/>
					{/if}
					{#each moreShown as name (name)}{@render term(name)}{/each}
				</div>
			</details>
		{/if}
	{/if}
</div>
