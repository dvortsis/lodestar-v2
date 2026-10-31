<script lang="ts">
	// An import's markers (§10.9.2 events) as a list: each is reachable by keyboard and reads as one
	// sentence. Renders nothing without an import in the range.
	import { formatDateFull } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { importText, isImport, type Annotation } from './imports';

	let { annotations, tz }: { annotations: readonly Annotation[]; tz: string } = $props();
	const locale = $derived(currentLocale());
	const marks = $derived(annotations.filter(isImport));
</script>

{#if marks.length}
	<div class="ins-card" data-testid="insights-import-markers">
		<h3 class="ins-card-title">{m.insights_import_markers()}</h3>
		<ol class="ins-list">
			{#each marks as a, i (a.at + a.kind + i)}
				{@const when = formatDateFull(a.at, locale, { timeZone: tz })}
				<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
				<li
					class="ins-item-row"
					tabindex="0"
					aria-label="{importText(a, locale)}, {when}"
					data-testid="insights-import-marker-{a.kind}"
				>
					<span>{importText(a, locale)}</span>
					<span class="ins-muted ins-num">{when}</span>
				</li>
			{/each}
		</ol>
	</div>
{/if}
