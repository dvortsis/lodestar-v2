<script lang="ts">
	// How strict a set of lists is (§2.9): the level, the server's plain warning and its figures.
	// Shown in the change dialog from the preview, and in the wizard for the lists chosen. The text
	// is the server's, rendered as text.
	import { formatCount, formatPercent } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { levelName, type Strictness } from './lists';

	let { strictness, id }: { strictness: Strictness; id?: string } = $props();
	const locale = () => currentLocale();
</script>

<section
	class="flex flex-col gap-2 rounded-box border border-field-border p-3"
	aria-label={m.blocklists_strictness_label()}
	data-testid="blocklists-strictness"
	{id}
>
	<p class="flex flex-wrap items-center gap-2">
		<span class="font-semibold">{m.blocklists_strictness_heading()}</span>
		<Badge variant={strictness.level === 'none' ? 'neutral' : 'warning'}
			><span data-testid="blocklists-level">{levelName(strictness.level)}</span></Badge
		>
	</p>
	{#if strictness.text}<p data-testid="blocklists-strictness-text">{strictness.text}</p>{/if}
	<p class="text-small text-muted-text" data-testid="blocklists-strictness-figures">
		{m.blocklists_strictness_figures({
			hashes: formatCount(strictness.hashes, locale()),
			share: formatPercent(strictness.ipv4_public_share, locale())
		})}{strictness.estimate ? ` ${m.blocklists_strictness_estimate()}` : ''}
	</p>
</section>
