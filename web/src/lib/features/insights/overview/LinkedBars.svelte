<script lang="ts" module>
	export interface BarItem {
		key: string;
		label: string;
		href?: string;
		/** Raw value for the bar's length; null draws no bar. */
		value: number | null;
		/** The formatted value ("—" for unknown). */
		text: string;
		/** A second, muted value (conversion, share). */
		sub?: string;
		/** Hatched: an incomplete cohort. */
		hatched?: boolean;
		tag?: string;
	}
</script>

<script lang="ts">
	// Horizontal bars that start at zero, in the order given, each optionally a real link. Lengths
	// come from the raw value (geometry only); the number shown arrives already formatted. Built
	// here because `HBarList` re-sorts its rows and has no link.
	import IconWarning from '~icons/ph/circle-half';
	import { resolve } from '$app/paths';
	import { lengthPct } from '$lib/charts/shared';
	import type { Route } from './href';

	interface Props {
		items: BarItem[];
		/** The value a full bar stands for. */
		max: number;
		label: string;
		ordered?: boolean;
		/** Id of a description every link points at. */
		describedby?: string;
		testid: string;
	}
	let { items, max, label, ordered = false, describedby, testid }: Props = $props();
</script>

{#snippet rows()}
	{#each items as item (item.key)}
		<li class="ins-item" data-testid="{testid}-{item.key}">
			<div class="ins-item-row">
				<span>
					{#if item.href}
						<a
							class="ins-link target"
							href={resolve(item.href as Route)}
							aria-describedby={describedby}>{item.label}</a
						>
					{:else}
						{item.label}
					{/if}
					{#if item.tag}
						<span class="ins-tag" data-testid="{testid}-{item.key}-tag"
							><IconWarning class="ins-icon-s" aria-hidden="true" />{item.tag}</span
						>
					{/if}
				</span>
				<span class="ins-num">
					<span data-testid="{testid}-{item.key}-value">{item.text}</span>
					{#if item.sub}
						<span class="ins-muted" data-testid="{testid}-{item.key}-sub">{item.sub}</span>
					{/if}
				</span>
			</div>
			<div class="ins-track" aria-hidden="true">
				{#if item.hatched}
					<div class="ins-fill ins-fill-hatched" style:width={lengthPct(item.value, max)}>
						<svg class="ins-hatch" focusable="false"
							><rect width="100%" height="100%" fill="url(#ins-hatch)" /></svg
						>
					</div>
				{:else}
					<div class="ins-fill" style:width={lengthPct(item.value, max)}></div>
				{/if}
			</div>
		</li>
	{/each}
{/snippet}

{#if ordered}
	<ol class="ins-list" aria-label={label} data-testid={testid}>{@render rows()}</ol>
{:else}
	<ul class="ins-list" aria-label={label} data-testid={testid}>{@render rows()}</ul>
{/if}
