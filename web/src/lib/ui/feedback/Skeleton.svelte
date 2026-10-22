<script lang="ts">
	interface Props {
		/** Stacked text-line placeholders; 0 renders a single block sized by `class`. */
		lines?: number;
		class?: string;
	}

	let { lines = 0, class: className = '' }: Props = $props();
</script>

<!-- Decorative: the loading region carries aria-busy or a visible status; this adds no noise. -->
{#if lines > 0}
	<div class="flex flex-col gap-2 {className}" aria-hidden="true" data-testid="skeleton">
		{#each Array.from({ length: lines }, (_, n) => n) as i (i)}
			<div
				class="h-4 rounded-selector bg-selected motion-safe:animate-pulse {i === lines - 1 &&
				lines > 1
					? 'w-2/3'
					: 'w-full'}"
			></div>
		{/each}
	</div>
{:else}
	<div
		class="rounded-selector bg-selected motion-safe:animate-pulse {className}"
		aria-hidden="true"
		data-testid="skeleton"
	></div>
{/if}
