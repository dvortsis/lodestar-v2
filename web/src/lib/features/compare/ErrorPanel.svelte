<script lang="ts">
	// Errors the user can fix from here: the comparison's torrents with a Remove each (the same
	// control as a column's), and a way back to Search. The server names no torrent for a 404.
	import IconRemove from '~icons/ph/x';
	import Button from '$lib/ui/controls/Button.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		title: string;
		text: string;
		hashes: string[];
		onRemove: (hash: string) => void;
		testid: string;
	}
	let { title, text, hashes, onRemove, testid }: Props = $props();
</script>

<section class="cmp-panel" aria-labelledby="compare-error-title" data-testid={testid}>
	<h2 id="compare-error-title" class="cmp-title">{title}</h2>
	<p class="cmp-text">{text}</p>
	{#if hashes.length > 0}
		<ul class="cmp-hashes">
			{#each hashes as hash (hash)}
				<li data-hash={hash}>
					<Mono wrap>{hash}</Mono>
					<Button
						variant="quiet"
						dense
						aria-label={m.compare_remove_for({ name: hash })}
						onclick={() => onRemove(hash)}
						data-first-action
					>
						<IconRemove aria-hidden="true" class="size-4" />{m.compare_remove()}
					</Button>
				</li>
			{/each}
		</ul>
	{/if}
	<a href={resolve('/search')} class="btn btn-primary cmp-back {FOCUS_RING} {TARGET_MIN}"
		>{m.compare_back_to_search()}</a
	>
</section>

<style>
	.cmp-panel {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: calc(var(--spacing) * 3);
		max-width: var(--prose-measure);
	}
	.cmp-title {
		font-size: var(--text-title);
		line-height: var(--text-title--line-height);
		font-weight: var(--font-weight-semibold);
	}
	.cmp-text {
		font-size: var(--text-body);
		line-height: var(--text-body--line-height);
		color: var(--color-muted-text);
	}
	.cmp-hashes {
		display: flex;
		width: 100%;
		flex-direction: column;
		gap: calc(var(--spacing) * 1);
	}
	.cmp-hashes li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: calc(var(--spacing) * 3);
		border-bottom: var(--border) solid var(--color-base-300);
		padding-block: calc(var(--spacing) * 1);
	}
	.cmp-back {
		width: fit-content;
	}
</style>
