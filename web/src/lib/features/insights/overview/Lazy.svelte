<script lang="ts" generics="P extends Record<string, unknown>">
	// Loads a below-the-fold level when it is near the viewport or, failing that, when the browser is
	// idle, so the first view (bar, strip, KPIs, exceptions) ships without it (§10.1 budget). The
	// placeholder has the level's final height, so nothing shifts when it arrives.
	import type { Component } from 'svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		load: () => Promise<{ default: Component<P> }>;
		props: P;
		/** Class giving the placeholder its final height. */
		height: string;
	}
	let { load, props, height }: Props = $props();

	let Loaded = $state.raw<Component<P>>();
	let host = $state<HTMLElement>();
	let failed = $state(false);

	$effect(() => {
		if (Loaded || !host) return;
		let started = false;
		const go = () => {
			if (started) return;
			started = true;
			failed = false;
			load().then(
				(mod) => (Loaded = mod.default),
				() => (failed = true)
			);
		};
		const io = new IntersectionObserver((e) => e.some((x) => x.isIntersecting) && go(), {
			rootMargin: '800px'
		});
		io.observe(host);
		const idle =
			typeof requestIdleCallback === 'function'
				? requestIdleCallback(go, { timeout: 2000 })
				: (setTimeout(go, 500) as unknown as number);
		return () => {
			io.disconnect();
			if (typeof cancelIdleCallback === 'function') cancelIdleCallback(idle);
			else clearTimeout(idle);
		};
	});
</script>

<div bind:this={host}>
	{#if Loaded}
		<Loaded {...props} />
	{:else if failed}
		<div class="ins-chunk-error" role="alert" data-testid="insights-chunk-error">
			<span>{m.insights_overview_chunk_failed()}</span>
			<Button onclick={() => location.reload()}>{m.insights_overview_retry()}</Button>
		</div>
	{:else}
		<Skeleton class={height} />
	{/if}
</div>
