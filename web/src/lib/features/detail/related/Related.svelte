<script lang="ts">
	// Related (§10.5 item 5; UI review 02 §5.1): four labelled lists with counts (other versions,
	// identical clones, same file, similar payloads), each row showing only what differs from the
	// open torrent, and "Compare these" pre-filling /compare with the open torrent and 1 to 4 picks.
	// Everything listed came from `GET /torrents/{hash}/siblings`, which applies the reader's
	// visibility (hidden, Show adult content, private): a 404 there shows nothing at all, and no row is
	// ever added on this side. Names are third-party text: rendered as text, bidi controls shown.
	import { formatBytes, formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { showBidi } from '$lib/util/bidi';
	import { TYPE_LABELS } from '$lib/util/content-labels';
	import { compareHref, torrentHref } from '$lib/features/compare/href';
	import type { SearchHit, TorrentDetail } from '../cache';
	import { tick } from 'svelte';
	import { loadSiblings, type Loaded } from './load';
	import {
		COMPARE_MAX,
		base,
		compareBlock,
		compareSet,
		differs,
		lists,
		type Attr,
		type List,
		type ListKey,
		type Unavailable
	} from './lists';
	import './related.css';

	interface Props {
		detail: TorrentDetail;
		hit: SearchHit | null;
		level?: number;
		mode: 'drawer' | 'page';
	}
	let { detail, hit, level = 3, mode }: Props = $props();
	const id = $props.id();
	const locale = $derived(currentLocale());

	let loaded = $state.raw<Loaded>({ status: 'loading' });
	let attempt = $state(0);
	let heading = $state<HTMLElement>();
	$effect(() => {
		void attempt;
		return loadSiblings(detail.hash, (s) => (loaded = s));
	});
	// Retry replaces the failed block (and its button) with the loading one: keep focus in the section.
	async function retry() {
		attempt++;
		await tick();
		heading?.focus();
	}

	const shown = $derived(loaded.status === 'ready' ? lists(loaded.siblings, detail.hash) : []);
	const open = $derived(base(detail, hit));
	/** Picked hashes, in the order they were picked (the /compare column order). */
	let picked = $state.raw<string[]>([]);
	// A new torrent starts with nothing picked (DetailView remounts on a new load; this is belt and braces).
	$effect.pre(() => {
		void detail.hash;
		picked = [];
	});
	const block = $derived(compareBlock(compareSet(detail.hash, picked).length - 1));
	const anyRows = $derived(shown.some((l) => l.hits.length > 0));
	const href = $derived(compareHref(compareSet(detail.hash, picked)));

	function toggle(hash: string, on: boolean) {
		picked = on ? [...picked.filter((h) => h !== hash), hash] : picked.filter((h) => h !== hash);
	}

	const LABELS: Record<ListKey, () => string> = {
		other_versions: m.detail_related_other_versions,
		identical_payload: m.detail_related_identical_payload,
		same_file: m.detail_related_same_file,
		similar_payload: m.detail_related_similar_payload
	};
	const WHY: Partial<Record<Unavailable, () => string>> = {
		no_title: m.detail_related_why_no_title,
		no_content_files: m.detail_related_why_no_content_files,
		needs_v2_files: m.detail_related_why_needs_v2_files,
		payload_not_specific: m.detail_related_why_payload_not_specific,
		partial_file_list: m.detail_related_why_partial_file_list,
		few_major_files: m.detail_related_why_few_major_files,
		search_disabled: m.detail_related_why_search_disabled
	};
	// The rest (an index without the field, a store without the history) all mean the index has to
	// catch up first.
	const why = (r: Unavailable) => (WHY[r] ?? m.detail_related_why_index)();
	const count = (l: List) => {
		const n = formatCount(l.hits.length, locale);
		return l.truncated ? m.detail_related_count_more({ count: n }) : n;
	};

	/** A differing attribute's text, or null when the row has no value to show for it. */
	function fact(a: Attr, h: SearchHit): string | null {
		switch (a) {
			case 'title':
				return h.title ? showBidi(h.title) : null;
			case 'resolution':
				return h.resolution ?? null;
			case 'content_type':
				return h.content_type ? (TYPE_LABELS[h.content_type]?.() ?? null) : null;
			case 'size':
				return formatBytes(h.size, locale);
			case 'files':
				return h.files === 1
					? m.detail_related_files_one()
					: m.detail_related_files({ count: formatCount(h.files, locale) });
			case 'seeders':
				// An estimate keeps its ≈ (§10.14), as on result rows.
				if (h.seeders === null) return m.detail_related_never_scraped();
				if (h.seeders === 1 && !h.seeders_approx) return m.detail_related_seeders_one();
				return m.detail_related_seeders({
					count: formatCount({ value: h.seeders, approx: h.seeders_approx }, locale)
				});
		}
	}
	const facts = (h: SearchHit) =>
		differs(h, open)
			.map((a) => [a, fact(a, h)] as const)
			.filter((f): f is readonly [Attr, string] => f[1] != null);

	// In the drawer a row opens in the drawer (one history entry, as j/k does); on /t it's a link.
	// The new load unmounts this section, link and all, so focus moves to the drawer's title first
	// (Esc and the keys keep working). `open.ts` is the drawer's own module, already loaded there;
	// importing it here on demand keeps it out of /t, which never calls it.
	function go(event: MouseEvent, h: SearchHit) {
		if (mode !== 'drawer' || event.button !== 0) return;
		if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
		event.preventDefault();
		const host = (event.currentTarget as HTMLElement).closest('[data-testid="drawer"]');
		host?.querySelector<HTMLElement>('h2[tabindex]')?.focus();
		void import('../open').then((o) => o.openDetail(h.hash, { hit: h }));
	}
</script>

{#if loaded.status !== 'withheld'}
	<section class="ls-detail-section" aria-labelledby="{id}-h" data-testid="detail-related">
		<div role="heading" aria-level={level} id="{id}-h" tabindex="-1" bind:this={heading}>
			{m.detail_related_title()}
		</div>
		{#if loaded.status === 'loading'}
			<div role="status" aria-busy="true" aria-label={m.detail_related_loading()}>
				<Skeleton lines={4} />
			</div>
		{:else if loaded.status === 'failed'}
			<div class="ls-detail-row" data-testid="detail-related-failed">
				<p class="text-body">{m.detail_related_failed()}</p>
				<Button dense onclick={retry}>{m.detail_related_retry()}</Button>
			</div>
		{:else}
			{#if anyRows}<div class="ls-detail-row" data-testid="detail-related-compare-bar">
					{#if block}
						<Button dense disabled aria-describedby="{id}-why" data-testid="detail-related-compare"
							>{m.detail_related_compare()}</Button
						>
						<p class="text-small text-muted-text" id="{id}-why" data-testid="detail-related-why">
							{block === 'none'
								? m.detail_related_compare_none()
								: m.detail_related_compare_over({
										count: formatCount(picked.length + 1 - COMPARE_MAX, locale)
									})}
						</p>
					{:else}
						<!-- eslint-disable svelte/no-navigation-without-resolve -- compareHref() resolves -->
						<a
							{href}
							class="btn btn-sm btn-outline border-field-border {FOCUS_RING} {TARGET_MIN}"
							aria-describedby="{id}-why"
							data-testid="detail-related-compare">{m.detail_related_compare()}</a
						>
						<!-- eslint-enable svelte/no-navigation-without-resolve -->
						<p
							class="text-small text-muted-text"
							id="{id}-why"
							aria-live="polite"
							data-testid="detail-related-why"
						>
							{m.detail_related_compare_picked({ count: formatCount(picked.length, locale) })}
						</p>
					{/if}
				</div>{/if}
			{#each shown as list (list.key)}
				<div class="ls-detail-stack" data-testid="detail-related-list" data-list={list.key}>
					<div
						role="heading"
						aria-level={level + 1}
						class="ls-related-h"
						id="{id}-{list.key}"
						data-testid="detail-related-list-h"
					>
						{LABELS[list.key]()}
						{#if !list.reason}<span
								class="text-muted-text tabular-nums"
								data-testid="detail-related-count">{count(list)}</span
							>{/if}
					</div>
					{#if list.reason}
						<p class="text-small text-muted-text" data-testid="detail-related-reason">
							{why(list.reason)}
						</p>
					{:else if list.hits.length === 0}
						<p class="text-small text-muted-text" data-testid="detail-related-none">
							{m.detail_related_none()}
						</p>
					{:else}
						<ul class="ls-related-list" aria-labelledby="{id}-{list.key}">
							{#each list.hits as h (h.hash)}
								{@const link = torrentHref(h.hash)}
								{@const shownFacts = facts(h)}
								<li class="ls-related-row" data-testid="detail-related-row" data-hash={h.hash}>
									<input
										type="checkbox"
										class="checkbox checkbox-sm border-field-border pointer-coarse:size-(--target-min) {FOCUS_RING}"
										aria-label={m.detail_related_pick({ name: showBidi(h.name) })}
										checked={picked.includes(h.hash)}
										onchange={(e) => toggle(h.hash, e.currentTarget.checked)}
										data-testid="detail-related-pick"
									/>
									<div class="ls-related-body">
										{#if link}
											<!-- eslint-disable svelte/no-navigation-without-resolve -- torrentHref() resolves -->
											<a
												href={link}
												class="link ls-related-name {FOCUS_RING}"
												onclick={(e) => go(e, h)}
												data-testid="detail-related-name"><bdi>{showBidi(h.name)}</bdi></a
											>
											<!-- eslint-enable svelte/no-navigation-without-resolve -->
										{:else}
											<span class="ls-related-name" data-testid="detail-related-name"
												><bdi>{showBidi(h.name)}</bdi></span
											>
										{/if}
										{#if shownFacts.length > 0}
											<p class="text-small text-muted-text" data-testid="detail-related-differs">
												{#each shownFacts as [a, text], i (a)}{#if i > 0}&nbsp;<span
															aria-hidden="true">·</span
														>&nbsp;{/if}<bdi data-attr={a} data-testid="detail-related-{a}"
														>{text}</bdi
													>{/each}
											</p>
										{/if}
									</div>
								</li>
							{/each}
						</ul>
					{/if}
					{#if list.gap}
						<p class="text-small text-muted-text" data-testid="detail-related-gap">
							{list.gap === 'partial' ? m.detail_related_partial() : m.detail_related_capped()}
						</p>
					{/if}
				</div>
			{/each}
		{/if}
	</section>
{/if}
