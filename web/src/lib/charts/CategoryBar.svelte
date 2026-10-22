<script lang="ts">
	import { GROUP_COLOR, GROUP_ORDER, lengthPct, type Group } from './shared';

	interface Segment {
		group: Group;
		/** Server count for the group; zero and unknown groups draw nothing. */
		value: number | null;
		/** Visible name for the group, e.g. "video". */
		label: string;
	}
	interface Props {
		segments: Segment[];
		/** Text summary for assistive tech: "82% video, 11% audio, …" (composed by the caller). */
		summary: string;
		/** Visible dominant-share text, e.g. "82% video". Colour is never the only cue. */
		dominant?: string;
		/**
		 * Shared-scale mode (Compare): the common maximum total. The bar fills `total / max` of
		 * its container, so bars from different torrents are comparable. Omit to fill the width.
		 */
		max?: number;
		testid?: string;
	}
	let { segments, summary, dominant, max, testid = 'category-bar' }: Props = $props();

	// Fixed order regardless of the order the server sent.
	const ordered = $derived(
		GROUP_ORDER.flatMap((g) => segments.filter((s) => s.group === g && (s.value ?? 0) > 0))
	);
	const total = $derived(ordered.reduce((sum, s) => sum + (s.value ?? 0), 0));
	const width = $derived(max === undefined ? '100%' : lengthPct(total, max));
</script>

<div class="flex min-w-0 flex-col gap-1" data-testid={testid}>
	<div
		class="flex h-2 gap-(--compbar-gap) bg-base-100"
		role="img"
		aria-label={summary}
		style:width
		data-testid="{testid}-bar"
	>
		{#each ordered as s, i (s.group)}
			<span
				class="h-full min-w-px {i === 0 ? 'rounded-l-selector' : ''} {i === ordered.length - 1
					? 'rounded-r-selector'
					: ''}"
				style:flex-grow={s.value}
				style:flex-basis="0"
				style:background-color={GROUP_COLOR[s.group]}
				data-group={s.group}
			></span>
		{/each}
	</div>
	{#if dominant}<span class="text-small text-muted-text" data-testid="{testid}-dominant"
			>{dominant}</span
		>{/if}
</div>
