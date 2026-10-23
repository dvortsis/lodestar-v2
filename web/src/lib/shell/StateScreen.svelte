<script lang="ts">
	// A full-page state (unreachable, not found, error): the explanation and the one next step come
	// first, the illustration follows (§10.19). Above x-large it sits beside the text (5/7 columns);
	// below, above it.
	import type { Snippet } from 'svelte';
	import type { SceneId } from '$lib/brand/scenes.generated';

	interface Props {
		title: string;
		text: string;
		/** Small mono line, e.g. "Error 500". */
		code?: string;
		scene?: SceneId;
		actions: Snippet;
	}
	let { title, text, code, scene, actions }: Props = $props();

	// The art loads on demand: its srcsets name every brand raster, which must stay out of the
	// first-load bundle (size-limit gate).
	let Scene = $state<typeof import('./StateScene.svelte').default>();
	$effect(() => {
		if (scene) void import('./StateScene.svelte').then((m) => (Scene = m.default));
	});
</script>

<section
	class="mx-auto grid w-full max-w-5xl items-center gap-6 p-6 x-large:grid-cols-12 x-large:gap-8"
	data-testid="state-screen"
>
	<div class="flex flex-col gap-4 x-large:col-span-7">
		<h1 class="text-display font-semibold">{title}</h1>
		<p class="max-w-(--prose-measure) text-body-lg text-muted-text">{text}</p>
		{#if code}<p class="font-mono text-small text-muted-text">{code}</p>{/if}
		<div class="flex flex-wrap gap-2">{@render actions()}</div>
	</div>
	{#if scene}
		<div
			class="ls-scene-slot order-first flex min-h-(--art-height-small) justify-center large:min-h-(--art-height-large) x-large:order-none x-large:col-span-5"
		>
			{#if Scene}<Scene id={scene} />{/if}
		</div>
	{/if}
</section>

<style>
	@media (max-height: 599.98px) {
		.ls-scene-slot {
			display: none;
		}
	}
</style>
