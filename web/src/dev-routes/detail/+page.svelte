<script lang="ts">
	// Test-only harness for the detail drawer (S-detail). It mounts DetailHost the way the search
	// page does (a lazy glob import) and has buttons that open a torrent, plus the raw viewer.
	import { onMount, type Component } from 'svelte';
	import { openDetail, closeDetail } from '$lib/features/detail/open';
	import RawViewer from '$lib/features/detail/raw/RawViewer.svelte';
	import { setCommandTarget } from '$lib/features/detail/commands';

	const loaders = import.meta.glob('$lib/features/detail/DetailHost.svelte');
	let Host = $state<Component | null>(null);
	onMount(async () => {
		const load = Object.values(loaders)[0];
		Host = ((await load?.()) as { default: Component } | undefined)?.default ?? null;
	});

	const A = 'a'.repeat(40);
	const B = 'b'.repeat(40);
	let rowA = $state<HTMLButtonElement>();

	const sample = {
		name: 'sample',
		'piece length': 262144,
		note: '<scr' + 'ipt>alert(1)</scr' + 'ipt>',
		long: 'x'.repeat(500),
		files: [{ length: 1, path: ['a', 'b.txt'] }],
		deep: { a: { b: { c: { d: { e: { f: { g: { h: { i: 1 } } } } } } } } }
	};
</script>

<main class="relative flex min-h-screen bg-base-200 text-base-content">
	<div class="flex min-w-0 flex-1 flex-col gap-4 p-6">
		<h1 class="text-display font-semibold">Detail drawer</h1>
		<div class="flex flex-wrap gap-3">
			<button
				bind:this={rowA}
				type="button"
				class="btn"
				data-testid="open-a"
				onmouseenter={() => setCommandTarget(A)}
				onclick={() => openDetail(A, { returnFocus: () => rowA })}>Open torrent A</button
			>
			<button
				type="button"
				class="btn"
				data-testid="open-b"
				onclick={() => openDetail(B, { replace: true })}>Swap to torrent B</button
			>
			<button type="button" class="btn" data-testid="close" onclick={closeDetail}>Close</button>
		</div>
		<section aria-labelledby="h-raw" class="flex flex-col gap-2">
			<h2 id="h-raw" class="text-title font-semibold">Raw viewer</h2>
			<RawViewer value={sample} maxDepth={6} maxString={40} />
		</section>
	</div>
	{#if Host}<Host />{/if}
</main>
