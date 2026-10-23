<script lang="ts">
	// The file comparison (§10.6): disclosures, collapsed by default, headed with the server's
	// `count`. The server sends at most 2,000 rows a list; rows outside the viewport are skipped by
	// `content-visibility`, so a long list scrolls smoothly without a virtual scroller (no extra JS).
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { formatBytes, formatCount } from '$lib/format';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { torrentHref } from './href';
	import { diffSections, type CompareResponse, type DiffSection } from './model';

	let { data }: { data: CompareResponse } = $props();
	const locale = $derived(currentLocale());
	const sections = $derived(diffSections(data));
	let opened = $state.raw<Record<string, boolean>>({});

	function toggled(s: DiffSection, open: boolean) {
		opened = { ...opened, [s.id]: open };
	}
	const filesText = (s: DiffSection) =>
		(s.list.count === 1 ? m.compare_files_one : m.compare_files_many)({
			count: formatCount(s.list.count, locale)
		});
	// The "common" list has no owner: the first torrent has every file in it.
	const openHash = (s: DiffSection) => s.hash ?? data.torrents[0]?.hash ?? '';
</script>

<section class="cmp-files-block" aria-labelledby="compare-diff-heading" data-testid="compare-diff">
	<h2 id="compare-diff-heading" class="cmp-title">{m.compare_diff_title()}</h2>
	{#if data.torrents.length > 2}<p class="cmp-note">{m.compare_diff_partial_note()}</p>{/if}
	{#each sections as s (s.id)}
		<details ontoggle={(e) => toggled(s, e.currentTarget.open)} data-testid="compare-diff-{s.id}">
			<summary>
				{s.title} · <span data-testid="compare-diff-{s.id}-count">{filesText(s)}</span>
			</summary>
			{#if opened[s.id] && s.list.files.length > 0}
				<ul>
					{#each s.list.files as f, i (i)}
						<li class="cmp-file">
							<Mono wrap>{f.path}</Mono>
							<span class="cmp-size">{formatBytes(f.size, locale)}</span>
						</li>
					{/each}
				</ul>
			{/if}
		</details>
		{#if s.list.truncated}
			{@const href = torrentHref(openHash(s))}
			<p class="cmp-note" data-testid="compare-diff-{s.id}-truncated">
				{m.compare_diff_truncated()}
				{#if href}<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
						{href}
						class="cmp-link">{m.compare_open_torrent()}</a
					>{/if}
			</p>
		{/if}
	{/each}
</section>

<style>
	.cmp-files-block {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 2);
		margin-top: calc(var(--spacing) * 8);
	}
	.cmp-title {
		font-size: var(--text-title);
		line-height: var(--text-title--line-height);
		font-weight: var(--font-weight-semibold);
	}
	.cmp-note {
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		color: var(--color-muted-text);
	}
	.cmp-link {
		color: var(--color-primary);
		text-decoration: underline;
	}
	details {
		border-bottom: var(--border) solid var(--color-base-300);
		padding-bottom: calc(var(--spacing) * 2);
	}
	summary {
		cursor: pointer;
		padding-block: calc(var(--spacing) * 2);
		font-weight: var(--font-weight-medium);
	}
	@media (pointer: coarse) {
		summary {
			min-height: var(--target-min);
		}
	}
	summary:focus-visible {
		outline-width: var(--focus-ring-width);
		outline-style: solid;
		outline-color: var(--color-primary);
		outline-offset: var(--focus-ring-width);
	}
	.cmp-file {
		content-visibility: auto;
		contain-intrinsic-size: auto var(--row-height-compact);
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: calc(var(--spacing) * 4);
		padding-block: calc(var(--spacing) * 1);
	}
	.cmp-size {
		flex: none;
		text-align: right;
		font-size: var(--text-small);
		line-height: var(--text-small--line-height);
		color: var(--color-muted-text);
		font-variant-numeric: tabular-nums;
	}
</style>
