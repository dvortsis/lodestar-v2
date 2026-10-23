<script lang="ts">
	// The app frame (§10.3): the main landmark and, once the page has painted, the chrome around it.
	// Only this frame is on the critical path; the top bar, skip link, event stream, palette and
	// toaster (`Chrome.svelte`) load after first paint into a slot that already has the bar's height,
	// so nothing shifts (§10.1: LCP, CLS). The page itself is rendered once, here, never remounted.
	import { onMount, type Snippet } from 'svelte';
	import { page } from '$app/state';
	import { isBareRoute } from './chrome';
	import { statusRef } from './status-ref.svelte';

	let { children }: { children: Snippet } = $props();

	const bare = $derived(isBareRoute(page.url.pathname));
	let Chrome = $state<typeof import('./Chrome.svelte').default>();
	let Connection = $state<typeof import('./ConnectionView.svelte').default>();
	const connection = $derived(statusRef.current?.connection ?? 'ok');

	// The UI font starts loading right after the first frame instead of competing with the scripts and
	// CSS that paint it (LCP, §10.1); the page paints in the fallback and swaps (font-display: swap).
	function preloadFont() {
		const link = document.createElement('link');
		link.rel = 'preload';
		link.as = 'font';
		link.type = 'font/woff2';
		link.crossOrigin = 'anonymous';
		link.href = '/fonts/atkinson-hyperlegible-next-latin.woff2';
		document.head.append(link);
	}

	onMount(() => {
		// After the first frame, so the request never competes with the page's first paint.
		let timer: ReturnType<typeof setTimeout>;
		const frame = requestAnimationFrame(() => {
			preloadFont();
			timer = setTimeout(
				() => void import('./Chrome.svelte').then((mod) => (Chrome = mod.default))
			);
		});
		return () => {
			cancelAnimationFrame(frame);
			clearTimeout(timer);
		};
	});

	$effect(() => {
		if (connection !== 'ok' && !Connection)
			void import('./ConnectionView.svelte').then((mod) => (Connection = mod.default));
	});
</script>

<div class="flex min-h-dvh flex-col bg-base-200 text-base-content">
	{#if Chrome}
		<Chrome {bare} />
	{:else if !bare}
		<!-- Holds the top bar's place (1 px border + 56 px) until it arrives. -->
		<div class="border-b border-base-300 bg-base-100"><div class="min-h-14"></div></div>
	{/if}
	<main id="main" tabindex="-1" class="flex flex-1 flex-col outline-none">
		{#if !bare && connection === 'unreachable'}
			{#if Connection}<Connection kind="unreachable" />{/if}
		{:else}
			{#if !bare && connection === 'reconnecting' && Connection}
				<Connection kind="reconnecting" />
			{/if}
			{@render children()}
		{/if}
	</main>
</div>
