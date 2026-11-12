<script lang="ts">
	// The "Identified as" section (§10.5, ENR-13): what the enrichment plugins say the torrent is,
	// one block per plugin, the surest first. The API omits `identified_as` when there is nothing to
	// show (and for hidden, private or adult torrents), so then nothing renders. Every text came from
	// a third party or a stored record: it is rendered as text only, never markup, with bidirectional
	// controls made visible (showBidi) and each value isolated in <bdi> so it can't reorder what is
	// around it. A null `resolved_via` is never shown as "from the name": it can be a plugin or an
	// unreadable record, so it gets no line at all.
	import { m } from '$lib/paraglide/messages.js';
	import { showBidi } from '$lib/util/bidi';
	import { resolvedVia } from '../parsed/labels';
	import type { TorrentDetail } from '../cache';

	let { detail, level = 3 }: { detail: TorrentDetail; level?: number } = $props();
	const id = $props.id();
	const identified = $derived(detail.identified_as);
	const answers = $derived(identified?.alternatives ?? []);
	const via = $derived(identified?.resolved_via ?? null);
	// `lang` takes a BCP 47 tag; plugin text that isn't one is left off rather than passed on.
	const LANG = /^[a-z]{2,3}(-[A-Za-z0-9]{2,8}){0,3}$/;
	const lang = (tag: string) => (tag !== 'und' && LANG.test(tag) ? tag : undefined);
</script>

{#if identified && (answers.length > 0 || via)}
	<section
		class="ls-detail-section"
		aria-labelledby="{id}-h"
		data-section="identified"
		data-testid="detail-identified"
	>
		<div role="heading" aria-level={level} id="{id}-h">{m.detail_identified_title()}</div>
		{#if via}
			<p class="text-small text-muted-text" data-testid="detail-identified-via">
				{m.detail_resolved_via({ method: resolvedVia(via) })}
			</p>
		{/if}
		{#each answers as a, i (i)}
			<div class="ls-detail-stack" data-testid="detail-identified-answer">
				<p class="text-small text-muted-text">
					{m.detail_identified_by({ source: showBidi(a.source.name) })}
					· {m.detail_identified_confidence({ percent: a.confidence })}{#if a.chosen}
						· <span data-testid="detail-identified-chosen">{m.detail_identified_chosen()}</span
						>{/if}
				</p>
				<dl class="ls-detail-facts">
					{#if a.canonical_title}
						<dt>{m.detail_identified_title_row()}</dt>
						<dd data-testid="detail-identified-canonical">
							<bdi>{showBidi(a.canonical_title)}</bdi>
						</dd>
					{/if}
					{#if a.year}
						<dt>{m.detail_identified_year()}</dt>
						<dd>{a.year}</dd>
					{/if}
					{#if a.genres.length > 0}
						<dt>{m.detail_identified_genres()}</dt>
						<dd data-testid="detail-identified-genres">
							{#each a.genres as g, gi (gi)}{#if gi > 0},
								{/if}<bdi>{showBidi(g)}</bdi>{/each}
						</dd>
					{/if}
					{#if a.ids.length > 0}
						<dt>{m.detail_identified_ids()}</dt>
						<dd data-testid="detail-identified-ids">
							{#each a.ids as x, xi (xi)}{#if xi > 0},
								{/if}<bdi>{showBidi(x.scheme)}: {showBidi(x.value)}</bdi>{/each}
						</dd>
					{/if}
					{#if a.alt_titles.length > 0}
						<dt>{m.detail_identified_alt_titles()}</dt>
						<dd data-testid="detail-identified-alt">
							{#each a.alt_titles as t, ti (ti)}{#if ti > 0};
								{/if}<bdi lang={lang(t.language)}>{showBidi(t.title)}</bdi>{/each}
						</dd>
					{/if}
				</dl>
			</div>
		{/each}
	</section>
{/if}
