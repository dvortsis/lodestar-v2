<script lang="ts" generics="P extends Record<string, unknown>">
	// Loads a below-the-fold level when it is near the viewport or, failing that, when the browser is
	// idle, so the first view (bar, strip, KPIs, exceptions) ships without it (§10.1 budget). The
	// placeholder has the level's final height, so nothing shifts when it arrives.
	import type { Component } from 'svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';

	interface Props {
		load: () => Promise<{ default: Component<P> }>;
		props: P;
		/** Class giving the placeholder its final height. */
		height: string;
	}
	let { load, props, height }: Props = $props();

	let Loaded = $state.raw<Component<P>>();
	let host = $state<HTMLElement>();

	$effect(() => {
		if (Loaded || !host) return;
		let started = false;
		const go = () => {
			if (started) return;
			started = true;
			void load().then((m) => (Loaded = m.default));
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
	{:else}
		<Skeleton class={height} />
	{/if}
</div>
